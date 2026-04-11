#!/bin/bash
# Lonko follow: when the active window changes, move lonko to the new window.
# Uses kill-and-respawn + layout save/restore to avoid layout drift.

LAYOUT_DIR="$HOME/.cache/lonko-layouts"
mkdir -p "$LAYOUT_DIR"

# If lonko wrote this sentinel it's navigating intentionally;
# don't follow so Claude keeps the focus.
SENTINEL="$HOME/.cache/lonko-no-follow"
if [ -f "$SENTINEL" ]; then
    rm -f "$SENTINEL"
    exit 0
fi

# Skip follow for floating popups and lonko-tray (internal sessions)
CURRENT_SESSION=$(tmux display-message -p '#{session_name}')
case "$CURRENT_SESSION" in
    floating-*|lonko-tray) exit 0 ;;
esac

LONKO_PANE=$(tmux list-panes -aF "#{pane_id} #{pane_current_command}" \
    | awk '$2 == "lonko" {print $1}' | head -1)

[ -z "$LONKO_PANE" ] && exit 0

CURRENT_WIN=$(tmux display-message -p '#{window_id}')
LONKO_WIN=$(tmux list-panes -aF "#{pane_id} #{window_id}" \
    | awk -v p="$LONKO_PANE" '$1==p {print $2}' | head -1)

# Nothing to do if lonko is already in the current window
[ "$LONKO_WIN" = "$CURRENT_WIN" ] && exit 0

# Capture the current pane so lonko can auto-select the right session on start
tmux display-message -p '#{pane_id}' > "$HOME/.cache/lonko-focus-pane"

# Save the target window's current layout BEFORE adding lonko
# (so we can restore it when lonko leaves).
CURRENT_LAYOUT_FILE="$LAYOUT_DIR/${CURRENT_WIN}.layout"
tmux display-message -t "$CURRENT_WIN" -p '#{window_layout}' > "$CURRENT_LAYOUT_FILE"

# Kill lonko in the previous window (reflows distorted)
tmux kill-pane -t "$LONKO_PANE"

# Restore the previous window's layout to what it was BEFORE lonko arrived
# (undoes the distortion accumulated by the reflow).
OLD_LAYOUT_FILE="$LAYOUT_DIR/${LONKO_WIN}.layout"
if [ -f "$OLD_LAYOUT_FILE" ]; then
    tmux select-layout -t "$LONKO_WIN" "$(cat "$OLD_LAYOUT_FILE")" 2>/dev/null || true
    rm -f "$OLD_LAYOUT_FILE"
fi

# Create a new lonko in the current window (full-height column on the right, 22%)
tmux split-window -h -f -l 22% -t "$CURRENT_WIN" -d "lonko"
