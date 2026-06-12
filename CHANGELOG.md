## v0.28.1 (2026-06-12)

### Fix

- **bookmark**: keep cwd-keyed labels stable when an agent starts running

## v0.28.0 (2026-06-04)

### Feat

- **chat**: cross-host chat over SSH (lonko-channel v2)

## v0.27.0 (2026-06-03)

### Feat

- **worktree**: add resume picker to reopen Claude in a worktree

## v0.26.0 (2026-05-11)

### Feat

- **chat**: show agent name and branch in chat overlay title

## v0.25.1 (2026-05-09)

### Fix

- **chat**: wrap chat input across rows instead of overflowing

## v0.25.0 (2026-05-07)

### Feat

- **channel**: chat with agents via Claude Code Channels (v1)

## v0.24.3 (2026-05-06)

### Fix

- **ui**: hide panel after activating a tmux session

## v0.24.2 (2026-05-03)

### Fix

- **bookmarks**: capture cwd at modal open to avoid lost labels

## v0.24.1 (2026-05-02)

### Refactor

- **ui**: move merged PR `M` to avatar column

## v0.24.0 (2026-05-02)

### Feat

- **ui**: keep PR badge with blinking `M` after merge

## v0.23.0 (2026-05-02)

### Feat

- **ui**: show open-PR badge on agent cards with `o` to open

## v0.22.5 (2026-05-01)

### Refactor

- **ui**: extract layout + connector helpers from list::render
- **app**: extract classify_click for shared double-click logic
- **state**: collapse cache_pane_for_session pattern

## v0.22.4 (2026-04-30)

### Refactor

- **state**: extract apply_hook + HookEffect for testable state
- **app**: split on_key into modal/normal dispatchers
- **app**: collapse the inflight-guard pattern into a helper

## v0.22.3 (2026-04-30)

### Refactor

- **state**: group modal-feature fields into sub-structs
- **app**: extract panel and navigate modules from app.rs
- **app**: extract remote-tailnet plumbing to app/remote.rs

## v0.22.2 (2026-04-30)

### Fix

- **panel**: skip auto-show inside remote/<host> and floating-* sessions

## v0.22.1 (2026-04-30)

### Fix

- **panel**: give focus to lonko on manual super+s/super+a invocation

## v0.22.0 (2026-04-30)

### Feat

- **panel**: adopt stationary-sidebar model with attention auto-show

## v0.21.3 (2026-04-29)

### Fix

- **panel**: don't capture preferred width while lonko is parked

## v0.21.2 (2026-04-29)

### Fix

- **panel**: stop preferred-width drift from percentage truncation

## v0.21.1 (2026-04-29)

### Fix

- **panel**: persist preferred sidebar width as a percentage

## v0.21.0 (2026-04-29)

### Feat

- **panel**: persist user-preferred sidebar width across moves

## v0.20.1 (2026-04-29)

### Fix

- **panel**: pin sidebar width back to a fixed 25%

## v0.20.0 (2026-04-29)

### Feat

- **ui**: pin a NEEDS YOU section at the bottom of the sidebar

### Fix

- **panel**: refresh no-follow sentinel across both hook firings
- **panel**: preserve sidebar width with absolute columns, not percent

## v0.19.9 (2026-04-29)

### Fix

- **panel**: preserve sidebar width across window switches

### Perf

- **app**: defer transcript parse and git_branch off the event loop

## v0.19.8 (2026-04-29)

### Fix

- **app**: index click resolution into the visible list, not the raw one
- **follow**: recover from stale follow-script lock
- **remote**: hide remote/<host> wrappers and reap them on disable
- **app**: refresh last_prompt on Stop and debounce auto-hide during pane moves

### Perf

- **app**: move per-second active_pane poll off the event loop

## v0.19.7 (2026-04-29)

### Fix

- **follow**: time out the no-follow sentinel after 1 s

## v0.19.6 (2026-04-27)

### Fix

- **panel**: linus review punch list — guards, lockout window, and tray hygiene

## v0.19.5 (2026-04-27)

### Fix

- **panel**: debounce queued clicks during slow focus/attach actions

## v0.19.4 (2026-04-27)

### Fix

- **remote**: stop double-paneling and auto-hiding inside remote wrappers

## v0.19.3 (2026-04-27)

### Fix

- **panel**: auto-hide instead of auto-quit, and stable double-click guard

## v0.19.2 (2026-04-27)

### Fix

- **remote**: pin switch-client to the active client on multi-client setups

## v0.19.1 (2026-04-27)

### Perf

- **remote**: cut network churn so remote mode does not feed Wi-Fi storms

## v0.19.0 (2026-04-27)

### Perf

- **state**: refresh Sessions tab off the main thread

## v0.18.9 (2026-04-26)

### Fix

- **remote**: drop tailnet peers named "localhost" from discovery

## v0.18.8 (2026-04-26)

### Fix

- **remote**: resolve tailscale CLI when PATH lacks /usr/local/bin

## v0.18.7 (2026-04-26)

### Fix

- harden agent teardown and lifecycle handling for shared cwd

## v0.18.6 (2026-04-25)

### Fix

- **worktree**: clean up branch when closing agent without merged PR

## v0.18.5 (2026-04-25)

### Fix

- **new-agent**: silence has-session probes when picking unique name
- **app**: silence stderr on direct tmux invocations from TUI
- **tmux**: silence stderr on remaining TUI-context wrappers
- **worktree**: capture git stdout/stderr to prevent TUI corruption

## v0.18.4 (2026-04-24)

### Fix

- **tmux**: suppress stderr on direct switch-client calls

## v0.18.3 (2026-04-24)

### Fix

- **state**: converge pidless provisionals by pane after /clear

## v0.18.2 (2026-04-24)

### Fix

- **state**: reap local agents whose Claude process has exited

## v0.18.1 (2026-04-24)

### Fix

- **scripts**: run lonko as pane command so Ctrl-C closes the panel

## v0.18.0 (2026-04-24)

### Feat

- **ui**: PR picker, remote section in agents list, reap ghost remotes

## v0.17.2 (2026-04-23)

### Fix

- **focus**: use lonko's window and skip pre-move when target has lonko

## v0.17.1 (2026-04-23)

### Perf

- **focus**: pre-move lonko pane + skip unneeded switch-client (LONKO-55)

## v0.17.0 (2026-04-23)

### Feat

- **ui**: press e to expand subagents inline (LONKO-54)

## v0.16.5 (2026-04-23)

### Fix

- **state**: use most recent transcript for lifecycle-discovered sessions

## v0.16.4 (2026-04-22)

### Fix

- **state**: ignore /loop sentinels as last_prompt

## v0.16.3 (2026-04-22)

### Fix

- **transcript**: ignore runtime-injected user blocks for last_prompt

## v0.16.2 (2026-04-22)

### Fix

- **state**: don't let stale transcript reads clobber fresh prompt

## v0.16.1 (2026-04-22)

### Fix

- **follow**: skip follow into remote/<host> sessions (LONKO-53)

### Refactor

- **follow**: move lonko with join-pane to preserve state

## v0.16.0 (2026-04-22)

### Feat

- **remote**: Shift+R toggles remote support at runtime (LONKO-52)
- **remote**: pre-populate idle remote agents via tmux polling
- **logs**: file-backed tracing + explicit remote-hook logging
- **remote**: reuse remote/<host> tmux session for attach
- **remote**: open remote attach as a top-level tmux session
- **remote**: attach to remote agent on Enter / double-click

### Fix

- **remote**: honor the no-follow sentinel on macOS
- **remote**: lowercase Tailnet hostnames to match hook --remote-tag
- **remote**: keep remote agents visible across attach + speed up first bridge
- **follow**: suppress lonko-follow on Agents-tab double-click focus

## v0.15.0 (2026-04-22)

### Feat

- **remote**: distinguish remote agents visually in the Agents tab
- **remote**: SSH reverse-tunnel bridge for remote Claude hooks
- **remote**: split lonko-hook socket path by invocation
- **remote**: stamp hook events with --remote-tag host
- **remote**: add install-remote subcommand to provision Tailnet hosts

### Fix

- **remote**: make focus a no-op for remote agents in Agents tab
- **remote**: silence ssh post-quantum warning on internal calls
- **remote**: unlink stale bridge socket on remote before ssh -R
- **remote**: bind bridge socket under /tmp to work around macOS sshd sandbox
- **remote**: stop tmux_scanner from pruning remote sessions
- **config**: always read config from $HOME/.config/lonko
- **remote**: keep bridges alive regardless of the active tab
- **install-remote**: force net.git-fetch-with-cli so cargo respects user git config
- **ui**: stabilize within-group agent order in Agents tab

## v0.14.1 (2026-04-20)

### Fix

- ignore detached HEAD sentinel when reading branch from transcript

### Refactor

- remove dead code and fix clippy warnings

## v0.14.0 (2026-04-18)

### Feat

- **remote**: make Remote tab opt-in via config file
- **remote**: exponential backoff for unreachable hosts and x to exclude
- **remote**: Enter on remote session opens ssh+attach in new tmux window
- **remote**: add Remote tab with Tailnet host discovery and SSH polling

### Fix

- remove .max(1) from cards_fitting and translate Spanish string
- race condition in lonko-follow and collapsed-flags duplication
- **remote**: guard backoff shift against overflow on high fail_count
- **remote**: address second-round review and remaining round-1 items
- **remote**: address PR review — shell injection, stale hosts, minor issues

## v0.13.0 (2026-04-17)

### Feat

- **ui**: collapsible repo groups in Agents tab

## v0.12.1 (2026-04-17)

### Fix

- **tmux**: guard kill against own window, not own session

## v0.12.0 (2026-04-17)

### Feat

- **ui**: draw left gutter bar connecting agents of the same repo
- **ui**: replace subagent cards with a count badge on parent

### Fix

- **ui**: replace gutter column with separator-only group connector

### Refactor

- centralise Claude-specific paths in agents::claude

## v0.10.2 (2026-04-14)

### Fix

- **ui**: show repo name for trunk-branch agents in agents list

## v0.10.1 (2026-04-13)

### Fix

- **ui**: compact help popup for narrow sidebar panels

## v0.10.0 (2026-04-13)

### Feat

- **ui**: inline worktree and bookmark inputs on selected agent card
- **ui**: inline worktree branch input on selected agent card

## v0.8.0 (2026-04-13)

### Feat

- **worktree**: copy dotfiles and run direnv allow on new worktrees

## v0.7.1 (2026-04-13)

### Fix

- **tmux**: clear screen before launching claude in new sessions

## v0.7.0 (2026-04-13)

### Feat

- **ui**: add new agent creation flow via popup (LONKO-3)

## v0.6.1 (2026-04-13)

### Fix

- **ui**: track double-click by card identity instead of screen row

## v0.6.0 (2026-04-13)

### Feat

- **ui**: show branch-derived display name for grouped worktree agents

## v0.5.2 (2026-04-13)

### Fix

- **ui**: guard compute_scroll against zero-height terminal
- **ui**: unify scroll and hit-test logic between render and mouse handler
- **ui**: align mouse hit-testing with render layout for agent cards
- **ui**: show bookmark and prompt simultaneously on agent cards

## v0.5.1 (2026-04-13)

### Fix

- **ui**: truncate agent name and branch to fit card width
- **scripts**: increase sidebar width from 22% to 25%

## v0.5.0 (2026-04-12)

### Feat

- **ui**: add help popup with keybinding reference

### Fix

- **tmux**: kill window instead of session when pressing x
- **ui**: make x key respect active tab and fix own-session guard

## v0.4.1 (2026-04-11)

### Fix

- **ui**: align session cache numbering with display order (#11)
- **ui**: dim all header elements when lonko loses focus (#10)

## v0.4.0 (2026-04-11)

### Feat

- **tmux**: auto-cleanup merged branches when removing worktree with x (#9)
- **tmux**: press p on agent to create worktree from its PR branch (#8)
- **worktree**: copy .claude config to new worktree if missing (#7)

## v0.3.0 (2026-04-11)

### Feat

- **ui**: remove keybinding hints from footer status bar (#6)

## v0.2.0 (2026-04-11)

### Feat

- group agents by repo root in the agents list (#4)
- search filter for Sessions tab (LONKO-12) (#2)
- auto-quit lonko when it's the last pane in its tmux session (#1)

### Fix

- make git_root_valid_repo test independent of checkout dir name (#3)

## v0.11.0 (2026-04-15)

### Feat

- **ui**: replace subagent cards with a count badge on parent

### Refactor

- centralise Claude-specific paths in agents::claude

## v0.10.2 (2026-04-14)

### Fix

- **ui**: show repo name for trunk-branch agents in agents list

## v0.10.1 (2026-04-13)

### Fix

- **ui**: compact help popup for narrow sidebar panels

## v0.10.0 (2026-04-13)

### Feat

- **ui**: inline worktree and bookmark inputs on selected agent card

## v0.9.0 (2026-04-13)

### Feat

- **ui**: inline worktree branch input on selected agent card

## v0.8.0 (2026-04-13)

### Feat

- **worktree**: copy dotfiles and run direnv allow on new worktrees

## v0.7.1 (2026-04-13)

### Fix

- **tmux**: clear screen before launching claude in new sessions

## v0.7.0 (2026-04-13)

### Feat

- **ui**: add new agent creation flow via popup (LONKO-3)

## v0.6.1 (2026-04-13)

### Fix

- **ui**: track double-click by card identity instead of screen row

## v0.6.0 (2026-04-13)

### Feat

- **ui**: show branch-derived display name for grouped worktree agents

## v0.5.2 (2026-04-13)

### Fix

- **ui**: guard compute_scroll against zero-height terminal
- **ui**: unify scroll and hit-test logic between render and mouse handler
- **ui**: align mouse hit-testing with render layout for agent cards
- **ui**: show bookmark and prompt simultaneously on agent cards

## v0.5.1 (2026-04-13)

### Fix

- **ui**: truncate agent name and branch to fit card width
- **scripts**: increase sidebar width from 22% to 25%

## v0.5.0 (2026-04-12)

### Feat

- **ui**: add help popup with keybinding reference

### Fix

- **tmux**: kill window instead of session when pressing x
- **ui**: make x key respect active tab and fix own-session guard

## v0.4.1 (2026-04-11)

### Fix

- **ui**: align session cache numbering with display order (#11)
- **ui**: dim all header elements when lonko loses focus (#10)

## v0.4.0 (2026-04-11)

### Feat

- **tmux**: auto-cleanup merged branches when removing worktree with x (#9)
- **tmux**: press p on agent to create worktree from its PR branch (#8)
- **worktree**: copy .claude config to new worktree if missing (#7)

## v0.3.0 (2026-04-11)

### Feat

- **ui**: remove keybinding hints from footer status bar (#6)

## v0.2.0 (2026-04-11)

### Feat

- group agents by repo root in the agents list (#4)
- search filter for Sessions tab (LONKO-12) (#2)
- auto-quit lonko when it's the last pane in its tmux session (#1)

### Fix

- make git_root_valid_repo test independent of checkout dir name (#3)

## v0.1.0 (2026-04-11)

### Refactor

- rename project from shepherd to lonko
