#!/bin/bash
# Lonko follow: when the active window changes, move the lonko pane to the
# new window using `tmux join-pane`. Keeps the lonko process alive so its
# agents list and remote bridges survive intact across window switches.

# Debug trace: `touch ~/.cache/lonko-follow-debug` to enable, `rm` to
# disable. Appends to /tmp/lonko-follow.log. Intentionally a file flag
# rather than an env var — tmux hooks inherit tmux's environment, not
# the user's shell, so `export` wouldn't reach this script.
DEBUG_FLAG="$HOME/.cache/lonko-follow-debug"
SENTINEL="$HOME/.cache/lonko-no-follow"
debug_log() {
    [ -f "$DEBUG_FLAG" ] || return 0
    local sentinel_state
    if [ -f "$SENTINEL" ]; then sentinel_state=yes; else sentinel_state=no; fi
    printf '[%s] %-14s session=%s lonko_win=%s current_win=%s sentinel=%s\n' \
        "$(date +%H:%M:%S)" "$1" \
        "${CURRENT_SESSION:-?}" \
        "${LONKO_WIN:-?}" "${CURRENT_WIN:-?}" "$sentinel_state" \
        >> /tmp/lonko-follow.log
}

# Serialise concurrent invocations (client-session-changed and
# after-select-window can fire back-to-back). `mkdir` is atomic.
#
# Stale-lock recovery: a previous invocation killed before its EXIT
# trap ran (SIGKILL, OOM, system crash) leaves the directory behind
# permanently, and every later hook bails out — lonko stops following
# until the user manually deletes the lock. Normal execution is well
# under a second, so anything older than LOCK_STALE_SECONDS is a
# leftover. Reclaim it and retry the mkdir; if a sibling raced us to
# the recovery, our second mkdir loses and we exit cleanly.
LOCKDIR="$HOME/.cache/lonko-follow.lock"
LOCK_STALE_SECONDS=5
if ! mkdir "$LOCKDIR" 2>/dev/null; then
    LOCK_MTIME=$(stat -f %m "$LOCKDIR" 2>/dev/null || echo 0)
    NOW=$(date +%s)
    LOCK_AGE=$(( NOW - LOCK_MTIME ))
    if [ "$LOCK_AGE" -lt "$LOCK_STALE_SECONDS" ]; then
        debug_log "lock-held"
        exit 0
    fi
    rm -rf "$LOCKDIR"
    if ! mkdir "$LOCKDIR" 2>/dev/null; then
        debug_log "lock-stale-lost"
        exit 0
    fi
    debug_log "lock-stale-recovered"
fi
trap 'rm -rf "$LOCKDIR"' EXIT

LAYOUT_DIR="$HOME/.cache/lonko-layouts"
mkdir -p "$LAYOUT_DIR"

# If lonko wrote this sentinel it's navigating intentionally;
# don't follow so Claude keeps the focus.
#
# TTL: 1 s. Lonko's writer (`refresh_no_follow_sentinel_async`) only
# refreshes the file across ~200 ms to cover the two hooks that
# `switch-client` fires (`client-session-changed` then
# `after-select-window`), so anything older than that is a leftover
# from an earlier intentional move and must NOT suppress a fresh
# follow. Without this, a sentinel written when the user clicked an
# agent kept blocking subsequent unrelated tab switches: lonko stayed
# in the previous window and `cmd+shift+a` was the only way out.
if [ -f "$SENTINEL" ]; then
    NOW=$(date +%s)
    MTIME=$(stat -f %m "$SENTINEL" 2>/dev/null || echo "$NOW")
    AGE=$(( NOW - MTIME ))
    rm -f "$SENTINEL"
    if [ "$AGE" -lt 1 ]; then
        debug_log "sentinel-hit"
        exit 0
    fi
    debug_log "sentinel-stale"
fi

# Skip follow for floating popups, lonko-tray (internal sessions), and
# remote/* wrapper sessions. The remote wrappers already contain an ssh
# attach to a host that has its own lonko, so moving the local lonko in
# too ends with two panels visible in the same window (LONKO-53).
CURRENT_SESSION=$(tmux display-message -p '#{session_name}')
case "$CURRENT_SESSION" in
    floating-*|lonko-tray|remote/*)
        debug_log "skip-$CURRENT_SESSION"
        exit 0
        ;;
esac

LONKO_PANE=$(tmux list-panes -aF "#{pane_id} #{pane_current_command}" \
    | awk '$2 == "lonko" {print $1}' | head -1)

if [ -z "$LONKO_PANE" ]; then
    debug_log "no-lonko-pane"
    exit 0
fi

CURRENT_WIN=$(tmux display-message -p '#{window_id}')
LONKO_WIN=$(tmux list-panes -aF "#{pane_id} #{window_id}" \
    | awk -v p="$LONKO_PANE" '$1==p {print $2}' | head -1)

# Nothing to do if lonko is already in the current window
if [ "$LONKO_WIN" = "$CURRENT_WIN" ]; then
    debug_log "already-here"
    exit 0
fi

debug_log "will-move"

# Capture the current pane so lonko can auto-select the right session
tmux display-message -p '#{pane_id}' > "$HOME/.cache/lonko-focus-pane"

# Save the target window's layout BEFORE lonko arrives, so we can restore
# it when lonko later leaves this window.
CURRENT_LAYOUT_FILE="$LAYOUT_DIR/${CURRENT_WIN}.layout"
tmux display-message -t "$CURRENT_WIN" -p '#{window_layout}' > "$CURRENT_LAYOUT_FILE"

# Use the user's persisted sidebar width preference (written by lonko
# every few ticks when the panel is stable). The value is an absolute
# column count clamped to [20, 200] on the writer side. Falls back to
# 25% when the file is missing (first run, never resized).
#
# Reading the live `pane_width` here was tried and was unstable: the
# value drifts due to layout auto-balancing in some destination window
# configurations, and percentages also truncated monotonically. A
# persisted preference, only updated when the panel is NOT moving,
# avoids both classes of regression.
PREF_WIDTH_FILE="$HOME/.cache/lonko-width.col"
WIDTH_SPEC="25%"
if [ -f "$PREF_WIDTH_FILE" ]; then
    pref=$(cat "$PREF_WIDTH_FILE" 2>/dev/null)
    if [ -n "$pref" ] && [ "$pref" -gt 0 ]; then
        WIDTH_SPEC="$pref"
    fi
fi

# Move the lonko pane to the current window. Full-height column on
# the right; `-d` keeps focus on the user's working pane.
#
# Using join-pane (not kill + split-window) is the whole point of this
# design: the lonko process stays alive, so its agents list and remote
# bridges survive intact. The source window reflows to whatever tmux
# chooses; restoring a saved layout there gets the previous shape back.
tmux join-pane -d -h -f -l "$WIDTH_SPEC" -s "$LONKO_PANE" -t "$CURRENT_WIN" 2>/dev/null

# Restore the previous window's layout to what it was BEFORE lonko lived
# there (undoes the distortion the original split introduced).
OLD_LAYOUT_FILE="$LAYOUT_DIR/${LONKO_WIN}.layout"
if [ -f "$OLD_LAYOUT_FILE" ]; then
    tmux select-layout -t "$LONKO_WIN" "$(cat "$OLD_LAYOUT_FILE")" 2>/dev/null || true
    rm -f "$OLD_LAYOUT_FILE"
fi
