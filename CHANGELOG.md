## v0.31.0 (2026-04-11)

### Feat

- **install**: generate tmux config fragment

## v0.30.0 (2026-04-11)

### Feat

- **ui**: mouse wheel scroll in Agents and Sessions tabs
- **respond**: add `shepherd respond` subcommand for external permission responses
- **agents**: add kill agent and remove worktree keybindings
- **agents**: visually distinguish subagents in the agents list
- **sessions**: mouse click to expand/collapse and select windows
- **bookmark**: bookmark agents with custom notes on session cards
- **worktree**: native TUI input for branch name instead of tmux prompt
- **worktree**: enable 'g' keybinding in both Agents and Sessions tabs
- **worktree**: launch Claude Code on a git worktree from Sessions tab
- **install**: manage tmux integration scripts from this repo

### Fix

- **kill**: prevent x/X from killing shepherd's own session
- **panel**: retry shepherd startup detection up to 1.5s
- **permissions**: send keys without Enter and target any waiting session
- **tmux**: write no-follow sentinel and use canonical session order in cache

### Refactor

- **state**: extract session resolution and focus logic from App
- **app**: extract pure helpers and reduce cognitive complexity

## v0.25.0 (2026-04-10)

### Feat

- **startup**: --tab CLI arg to open on specific tab (agents/sessions)

## v0.24.1 (2026-04-09)

### Fix

- **panel**: restore window layout on hide_panel to avoid cumulative drift

## v0.24.0 (2026-04-09)

### Feat

- **sessions**: expand session card with space to navigate windows

## v0.23.0 (2026-04-09)

### Feat

- **shepherd**: smart window pills + h/l window navigation in sessions tab

## v0.22.0 (2026-04-09)

### Feat

- **mouse**: click on header switches between Agents/Sessions tabs

## v0.21.0 (2026-04-09)

### Feat

- **mouse**: double-click on Sessions tab does switch-client to tmux session

## v0.20.2 (2026-04-09)

### Fix

- **keys**: a/A=Agents, s/S=Sessions (upper and lowercase)

## v0.20.1 (2026-04-09)

### Fix

- **keys**: A/S directly switch to Agents/Sessions tab

## v0.20.0 (2026-04-09)

### Feat

- **sessions**: Sessions tab shows local tmux sessions

## v0.19.7 (2026-04-09)

### Refactor

- **ui**: fix clippy warnings in list.rs

## v0.19.6 (2026-04-09)

### Refactor

- **tabs**: rename Tab::Sessions→Agents, Tab::Activity→Sessions

## v0.19.5 (2026-04-09)

### Fix

- **mouse**: repeat select-pane at intervals to beat tmux mouse-mode

## v0.19.4 (2026-04-09)

### Fix

- **mouse**: use tmux run-shell for double-click focus

## v0.19.3 (2026-04-09)

### Fix

- **mouse**: generation counter to cancel stale spawns on double-click

## v0.19.2 (2026-04-09)

### Fix

- **mouse**: 80ms delay on double-click focus to avoid bounce

## v0.19.1 (2026-04-09)

### Fix

- **mouse**: hide panel after double-click and Enter to yield focus to agent

## v0.19.0 (2026-04-09)

### Feat

- **mouse**: double-click on agent focuses the Claude session

## v0.18.4 (2026-04-09)

### Fix

- **ui**: three background states for navigation cursor vs active session

## v0.18.3 (2026-04-09)

### Feat

- **ui**: keep emoji in avatar, move spinner to status line
- **ui**: use emoji as session avatar derived from project name
- remove process mode (p)
- **ui**: A/S underlined in tabs, simplified footer without shortcuts
- **ui**: remove space support, Enter only for focus
- **ui**: rename tabs to "Agents | Sessions"
- **shepherd**: leader+space shows popup with shortcuts+sessions, info cache
- **shepherd**: session numbering + leader+N for focus
- **shepherd**: detect Claude sessions via tmux scan on startup
- **ui**: tokyo night palette, avatar chips, left-stripe border, fix utf-8 panic in prompt truncation
- **focus**: Enter focuses the Claude panel directly
- **panel**: shepherd persists in shepherd-tray, opens panel with join-pane
- **ui**: arrow follows active pane, thick border is navigation cursor
- **focus**: header dims when shepherd loses pane focus
- **panel**: pin mode — Enter/spc focus without closing shepherd, q/Esc to exit
- **panel**: auto-select session of active pane when opening side panel
- **hook**: open side panel instead of popup when Claude needs attention
- **mouse**: mouse support to select sessions and close with click outside
- **ui**: animated spinner in Running state with throbber-widgets-tui
- **notifications**: desktop notification on WaitingForUser when Ghostty is not focused
- **processes**: Processes mode with p key to list and focus non-Claude tmux panes

### Fix

- **ui**: unique colors per agent position instead of project name hash
- **ui**: avoid duplicate icons across visible sessions
- **duplicates**: skip lifecycle session when pane already has a newer session via hook
- **duplicates**: evict old session from same pane when new conversation hook arrives
- **shepherd**: find pane by PID when writing cache for sessions without pane_id
- **shepherd**: select-pane after switch-client to navigate within the same session
- **shepherd**: align session cache with UI positions
- **shepherd**: number below avatar, not before it (no extra horizontal space)
- **shepherd**: context_max per model (opus=1M, sonnet/haiku=200K)
- **shepherd**: detect Claude via pgrep -x instead of pane_current_command
- **shepherd**: fix Claude pane detection and aggressive TmuxPaneGone
- **shepherd**: auto-prune completed sessions after 30 seconds
- **lifecycle**: skip stale session files with dead PIDs on startup
- **lifecycle**: deduplicate sessions by session_id to prevent duplicates when hook pre-creates session
- **hooks**: resolve race condition, use notification_type, add write timeout
- **ui**: initialize focused_session_id on startup and on session discovery
- **hooks**: capture prompt field from UserPromptSubmit to update last_prompt immediately
- **ui**: brighten header border to blue when shepherd has focus
- **ui**: retain focused session highlight when shepherd pane is active

### Refactor

- **app**: split handle_event into handlers + extract pure logic to AppState
- extract Session::apply_transcript_info + unit tests
- **shepherd**: unified dispatch in shortcut-jump.sh, remove install_tmux_bindings

## v0.29.0 (2026-04-10)

### Feat

- **respond**: add `shepherd respond` subcommand for external permission responses

### Fix

- **kill**: prevent x/X from killing shepherd's own session

## v0.28.0 (2026-04-10)

### Feat

- **agents**: add kill agent and remove worktree keybindings
- **agents**: visually distinguish subagents in the agents list
- **bookmark**: bookmark agents with custom notes on session cards

## v0.27.0 (2026-04-10)

### Feat

- **sessions**: mouse click to expand/collapse and select windows
- **worktree**: native TUI input for branch name instead of tmux prompt
- **worktree**: enable 'g' keybinding in both Agents and Sessions tabs
- **worktree**: launch Claude Code on a git worktree from Sessions tab

### Fix

- **panel**: retry shepherd startup detection up to 1.5s

## v0.26.0 (2026-04-10)

### Feat

- **install**: manage tmux integration scripts from this repo

### Fix

- **permissions**: send keys without Enter and target any waiting session
- **tmux**: write no-follow sentinel and use canonical session order in cache

### Refactor

- **state**: extract session resolution and focus logic from App
- **app**: extract pure helpers and reduce cognitive complexity

## v0.25.0 (2026-04-10)

### Feat

- **startup**: --tab CLI arg to open on specific tab (agents/sessions)

## v0.24.1 (2026-04-09)

### Fix

- **panel**: restore window layout on hide_panel to avoid cumulative drift

## v0.24.0 (2026-04-09)

### Feat

- **sessions**: expand session card with space to navigate windows

## v0.23.0 (2026-04-09)

### Feat

- **shepherd**: smart window pills + h/l window navigation in sessions tab

## v0.22.0 (2026-04-09)

### Feat

- **mouse**: click en header cambia entre tabs Agents/Sessions

## v0.21.0 (2026-04-09)

### Feat

- **mouse**: doble click en Sessions tab hace switch-client a sesión tmux

## v0.20.2 (2026-04-09)

### Fix

- **keys**: a/A=Agents, s/S=Sessions (mayúscula y minúscula)

## v0.20.1 (2026-04-09)

### Fix

- **keys**: A/S cambian directamente a tab Agents/Sessions

## v0.20.0 (2026-04-09)

### Feat

- **sessions**: tab Sessions muestra sesiones tmux locales

## v0.19.7 (2026-04-09)

### Refactor

- **ui**: fix clippy warnings en list.rs

## v0.19.6 (2026-04-09)

### Refactor

- **tabs**: renombrar Tab::Sessions→Agents, Tab::Activity→Sessions

## v0.19.5 (2026-04-09)

### Fix

- **mouse**: repetir select-pane en intervalos para vencer tmux mouse-mode

## v0.19.4 (2026-04-09)

### Fix

- **mouse**: usar tmux run-shell para focus de doble click

## v0.19.3 (2026-04-09)

### Fix

- **mouse**: generation counter para cancelar spawns obsoletos en doble click

## v0.19.2 (2026-04-09)

### Fix

- **mouse**: delay de 80ms en focus de doble click para evitar rebote

## v0.19.1 (2026-04-09)

### Fix

- **mouse**: hide_panel tras doble click y Enter para ceder foco al agente

## v0.19.0 (2026-04-09)

### Feat

- **mouse**: doble click en agente enfoca la sesión Claude

## v0.18.4 (2026-04-09)

### Fix

- **ui**: tres estados de fondo para cursor de navegación vs sesión activa

## v0.18.3 (2026-04-09)

### Fix

- **ui**: colores únicos por posición de agente en lugar de hash del proyecto

## v0.18.2 (2026-04-09)

### Refactor

- **app**: partir handle_event en handlers + extraer logica pura a AppState
- extraer Session::apply_transcript_info + tests unitarios

## v0.18.1 (2026-04-08)

### Fix

- **ui**: evitar íconos duplicados entre sesiones visibles

## v0.18.0 (2026-04-08)

### Feat

- **ui**: mantener emoji en avatar, mover spinner a status line

## v0.17.0 (2026-04-08)

### Feat

- **ui**: usar emojis como avatar de sesión derivado del nombre del proyecto

## v0.16.1 (2026-04-08)

### Fix

- **duplicates**: evitar sesion lifecycle cuando el pane ya tiene sesion mas nueva (hook)

## v0.16.0 (2026-04-08)

### Feat

- eliminar modo procesos (p)

## v0.15.0 (2026-04-08)

### Feat

- **ui**: A/S subrayados en tabs, footer simplificado sin shortcuts

## v0.14.0 (2026-04-08)

### Feat

- **ui**: eliminar soporte de space, solo Enter para focus

## v0.13.1 (2026-04-08)

### Fix

- **duplicates**: evict sesion vieja del mismo pane al llegar hook de nueva conversacion

## v0.13.0 (2026-04-08)

### Feat

- **ui**: renombrar tabs a "Agents | Sessions"

## v0.12.4 (2026-04-08)

### Fix

- **shepherd**: buscar pane por PID al escribir cache para sesiones sin pane_id

## v0.12.3 (2026-04-08)

### Fix

- **shepherd**: select-pane tras switch-client para navegar dentro de la misma sesión

## v0.12.2 (2026-04-08)

### Fix

- **shepherd**: alinear cache de sesiones con posiciones de la UI

## v0.12.1 (2026-04-08)

### Refactor

- **shepherd**: dispatch unificado en shortcut-jump.sh, eliminar install_tmux_bindings

## v0.12.0 (2026-04-08)

### Feat

- **shepherd**: leader+space muestra popup con shortcuts+sesiones, cache con info

## v0.11.1 (2026-04-08)

### Fix

- **shepherd**: numero debajo del avatar, no antes (sin consumir espacio horizontal)

## v0.11.0 (2026-04-08)

### Feat

- **shepherd**: numeracion de sesiones + leader+N para focus

## v0.10.3 (2026-04-08)

### Fix

- **shepherd**: context_max según modelo (opus=1M, sonnet/haiku=200K)

## v0.10.2 (2026-04-08)

### Fix

- **shepherd**: detectar claude via pgrep -x en lugar de pane_current_command

## v0.10.1 (2026-04-08)

### Fix

- **shepherd**: corregir detección de panes claude y TmuxPaneGone agresivo

## v0.10.0 (2026-04-08)

### Feat

- **shepherd**: detectar sesiones claude via scan tmux al arranque

## v0.9.8 (2026-04-08)

### Fix

- **shepherd**: auto-prune sesiones completadas después de 30 segundos

## v0.9.7 (2026-04-08)

### Fix

- **lifecycle**: skip stale session files with dead PIDs on startup

## v0.9.6 (2026-04-08)

### Fix

- **lifecycle**: deduplicate sessions by session_id to prevent duplicates when hook pre-creates session

## v0.9.5 (2026-04-08)

### Fix

- **hooks**: resolve race condition, use notification_type, add write timeout

## v0.9.4 (2026-04-08)

### Fix

- **ui**: initialize focused_session_id on startup and on session discovery

## v0.9.3 (2026-04-08)

### Fix

- **hooks**: capture prompt field from UserPromptSubmit to update last_prompt immediately

## v0.9.2 (2026-04-08)

### Fix

- **ui**: brighten header border to blue when shepherd has focus

## v0.9.1 (2026-04-08)

### Fix

- **ui**: retain focused session highlight when shepherd pane is active

## v0.9.0 (2026-04-08)

### Feat

- **ui**: tokyo night palette, avatar chips, left-stripe border, fix utf-8 panic in prompt truncation

## v0.8.0 (2026-04-07)

### Feat

- **focus**: Enter enfoca el panel de claude directamente

## v0.7.0 (2026-04-07)

### Feat

- **panel**: shepherd persiste en shepherd-tray, abre panel con join-pane

## v0.6.0 (2026-04-07)

### Feat

- **ui**: flecha sigue pane activo, borde grueso es cursor de navegacion
- **focus**: header se dimea cuando shepherd pierde el foco del pane
- **panel**: pin mode — Enter/spc enfocan sin cerrar shepherd, q/Esc para salir
- **panel**: auto-seleccionar sesion del pane activo al abrir el panel lateral
- **hook**: abrir panel lateral en vez de popup cuando Claude necesita atencion
- **mouse**: soporte de mouse para seleccionar sesiones y cerrar con clic fuera

## v0.5.0 (2026-04-07)

### Feat

- **ui**: spinner animado en estado Running con throbber-widgets-tui

## v0.4.0 (2026-04-07)

### Feat

- **notifications**: notificacion desktop al entrar en WaitingForUser si Ghostty no tiene foco

## v0.3.0 (2026-04-07)

### Feat

- **processes**: modo Processes con tecla p para listar y focalizar panes tmux no-Claude

## v0.2.0 (2026-04-07)

### Feat

- migrate from quick-terminal to tmux popup, scroll, prompt display, version header

## v0.1.0 (2026-04-07)

### Feat

- add --install-ghostty-config and fix Escape drawer close (SHP-19)
- **M6**: detail pane with transcript data (branch, model, last prompt, last tool)
- add WaitingForInput state distinct from WaitingForUser (permission)
- Enter focuses session and closes the drawer
- close quick terminal drawer with Esc
- auto-open quick terminal drawer when session needs attention
- M4+M5 focus and permission control
- self-contained hook installer (shepherd --install-hooks)
- M3 hook sink + state machine + install script
- M2 lifecycle watcher on ~/.claude/sessions/
- M1 skeleton TUI with fake session data

### Fix

- parse transcript user prompts as string or array content
- load branch/model from transcript on session discovery
- read transcript on detail open and session nav, not just on Stop
- only treat permission notifications as WaitingForUser, not input prompts
- focus pane on main tmux client, not the shepherd-tray quick terminal
- discover tmux pane by walking process tree when pane_id is unknown
- HookPayload uses snake_case (matches Claude Code hook events)
- use full path to shepherd-hook in settings.json
