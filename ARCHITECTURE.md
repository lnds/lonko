# Shepherd Architecture

Shepherd is a TUI that monitors Claude Code sessions running in tmux.
It shows session status, lets you navigate between them, and can grant
permission prompts without leaving your current work.

## Crate structure

```
crates/
  shepherd/          Main TUI binary
  shepherd-hook/     Lightweight forwarder (stdin -> Unix socket)
scripts/
  shepherd-panel.sh  Toggle side panel (installed to ~/.config/tmux/scripts/)
  shepherd-follow.sh Auto-follow between tmux windows (same)
```

### shepherd (main binary)

```
src/
  main.rs            Entry point, arg parsing (--tab, --install-hooks, focus)
  app.rs             App struct, event loop, pure helpers
  state.rs           AppState, Session, SessionStatus, navigation logic
  event.rs           Event enum (Tick, Key, Mouse, Hook, ...)
  focus.rs           `shepherd focus N` subcommand
  install.rs         Hook installer for ~/.claude/settings.json

  control/
    tmux.rs          tmux commands (select-pane, send-keys, list-panes, ...)
    ghostty.rs       Ghostty focus detection

  sources/
    hooks.rs         Unix socket listener for Claude Code hook events
    lifecycle.rs     Filesystem watcher for ~/.claude/sessions/
    tmux_scanner.rs  Periodic scan of tmux panes for Claude processes
    transcript.rs    JSONL transcript parser (model, prompt, tokens, ...)

  ui/
    mod.rs           Top-level render dispatch (header + body + footer)
    header.rs        Tab bar (Agents | Sessions)
    list.rs          Agent session cards (Agents tab)
    tmux_sessions.rs Tmux session cards (Sessions tab)
    detail.rs        Expanded detail view for a session
    footer.rs        Status bar + permission shortcuts (y/w/n)
```

### shepherd-hook

A fast (<10ms) CLI that reads a Claude Code hook event from stdin,
enriches it with `$TMUX_PANE`, and forwards it to shepherd via Unix
socket. If shepherd is not running and the event is `Notification` or
`Stop`, it opens the panel and retries.

## Data flow

### Session discovery

Sessions are discovered through three independent paths:

1. **Hook events** -- Claude Code fires hooks (SessionStart,
   UserPromptSubmit, PreToolUse, etc.) that `shepherd-hook` forwards
   via Unix socket. This is the primary and fastest path.

2. **Lifecycle files** -- A filesystem watcher monitors
   `~/.claude/sessions/` for new session files. This catches sessions
   that started before shepherd.

3. **tmux scanner** -- Every 5 seconds, shepherd scans all tmux panes
   for Claude processes. This catches sessions missed by hooks
   (e.g., remote sessions or sessions started without hooks).

All three paths converge into the `Event` channel and are processed
by `App::handle_event`.

### Permission flow

When Claude Code needs permission, it fires a `Notification` hook with
`notification_type: "permission_prompt"`. Shepherd sets the session
status to `WaitingForUser` and the footer shows `y:yes w:always n:no`.

When the user presses `y`/`w`/`n`, shepherd finds the first
`WaitingForUser` session (regardless of which session is selected) and
sends the corresponding key (`1`/`2`/`3`) to its tmux pane via
`tmux send-keys -l` (literal, no Enter -- Claude captures raw input).

### Communication via cache files

Shepherd and the tmux scripts communicate through files in the OS
cache directory (`~/Library/Caches/` on macOS, `~/.cache/` on Linux):

| File | Purpose |
|---|---|
| `shepherd-focus-pane` | Pane ID for auto-selection on startup |
| `shepherd-no-follow` | Sentinel: shepherd navigated intentionally, skip follow |
| `shepherd-sessions` | Pane IDs per line (for `shepherd focus N`) |
| `shepherd-sessions-info` | Session list, tab-separated (N, name, cwd) |
| `shepherd-layouts/<id>.layout` | Saved window layouts, keyed by window ID |

## Install

`install.sh` handles four things:

1. **Binaries**: `cargo install` for `shepherd` and `shepherd-hook`
   to `~/.cargo/bin/`
2. **tmux scripts**: copies `scripts/*.sh` to `~/.config/tmux/scripts/`
3. **tmux config fragment**: writes `~/.config/tmux/shepherd.conf` with
   all hooks, keybindings, and escape-sequence routing shepherd needs
4. **Claude hooks**: runs `shepherd --install-hooks` to configure
   `~/.claude/settings.json`

## tmux integration

After running `install.sh`, shepherd generates
`~/.config/tmux/shepherd.conf` with everything you need to wire it
into tmux. Add **one line** to your `tmux.conf` to source it:

```tmux
if-shell "[ -f ~/.config/tmux/shepherd.conf ]" "source-file ~/.config/tmux/shepherd.conf"
```

Place this line **after** any `set-hook -g client-session-changed`
line in your config, so shepherd's `-ga` (append) stacks correctly on
top of your existing hooks. The usual spot is right after the
appearance/hooks section near the top of `tmux.conf`.

The file is auto-generated on every `install.sh` run, so upgrading
shepherd automatically picks up new bindings. Do not edit it by
hand — customizations would be lost on the next install.

### What the generated file contains

**Auto-follow hooks** — shepherd re-parents to the active window when
you switch sessions or windows:

```tmux
set-hook -ga client-session-changed 'run-shell "~/.config/tmux/scripts/shepherd-follow.sh"'
set-hook -ga after-select-window    'run-shell "~/.config/tmux/scripts/shepherd-follow.sh"'
```

**Panel toggle** — `prefix + s` toggles the shepherd side panel:

```tmux
bind s run-shell "~/.config/tmux/scripts/shepherd-panel.sh"
```

**Direct tab access via terminal escape sequences** — if your terminal
sends `\e[sa` / `\e[ss`, shepherd opens the Agents or Sessions tab:

```tmux
set -s user-keys[22] "\e[sa"
set -s user-keys[23] "\e[ss"
bind -n User22 run-shell "~/.config/tmux/scripts/shepherd-panel.sh agents"
bind -n User23 run-shell "~/.config/tmux/scripts/shepherd-panel.sh sessions"
```

**Permission responses from any pane** — if your terminal sends
`\e[sy` / `\e[sn` / `\e[sw`, shepherd responds to the current
permission prompt without you having to focus the panel:

```tmux
set -s user-keys[24] "\e[sy"
set -s user-keys[25] "\e[sn"
set -s user-keys[26] "\e[sw"
bind -n User24 run-shell "shepherd respond y"
bind -n User25 run-shell "shepherd respond n"
bind -n User26 run-shell "shepherd respond w"
```

The `shepherd respond` subcommand talks to the running shepherd over
its Unix socket (same channel `shepherd-hook` uses), so this works
even when the panel isn't visible and regardless of which pane has
focus.

### Configuring your terminal

The escape sequences above (`\e[sa`, `\e[sy`, etc.) are arbitrary --
pick any that don't collide with your terminal's built-in sequences.
For Ghostty:

```
# ~/.config/ghostty/config
keybind = cmd+shift+a=text:\x1b[sa
keybind = cmd+shift+s=text:\x1b[ss
keybind = cmd+shift+y=text:\x1b[sy
keybind = cmd+shift+n=text:\x1b[sn
keybind = cmd+shift+w=text:\x1b[sw
```

This works with any terminal that can send arbitrary escape sequences
(Ghostty, WezTerm, Kitty, etc.). If your terminal doesn't send them,
the `user-keys` entries simply never fire — the rest of the config
keeps working.

### Optional: digit shortcuts for quick session focus

If you want digits 0-9 to jump directly to Claude sessions, you can
use `shepherd focus N` in your shortcut system. For example, in a
tmux keybinding:

```tmux
bind 1 run-shell "shepherd focus 1"
bind 2 run-shell "shepherd focus 2"
# ... etc
```

Shepherd writes the session list to the cache file
`shepherd-sessions-info` (tab-separated: number, project name, cwd)
which external scripts can read for display purposes.
