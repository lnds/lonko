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
