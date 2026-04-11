#!/bin/bash
# Shepherd follow: cuando cambia el window activo, mueve shepherd al nuevo window.
# Usa kill-and-respawn + layout save/restore para evitar drift en el layout.

LAYOUT_DIR="$HOME/.cache/shepherd-layouts"
mkdir -p "$LAYOUT_DIR"

# Si shepherd escribió este sentinel, es porque está navegando intencionalmente;
# no seguirlo para que Claude quede con el foco.
SENTINEL="$HOME/.cache/shepherd-no-follow"
if [ -f "$SENTINEL" ]; then
    rm -f "$SENTINEL"
    exit 0
fi

# Skip follow for floating popups and shepherd-tray (internal sessions)
CURRENT_SESSION=$(tmux display-message -p '#{session_name}')
case "$CURRENT_SESSION" in
    floating-*|shepherd-tray) exit 0 ;;
esac

SHEPHERD_PANE=$(tmux list-panes -aF "#{pane_id} #{pane_current_command}" \
    | awk '$2 == "shepherd" {print $1}' | head -1)

[ -z "$SHEPHERD_PANE" ] && exit 0

CURRENT_WIN=$(tmux display-message -p '#{window_id}')
SHEPHERD_WIN=$(tmux list-panes -aF "#{pane_id} #{window_id}" \
    | awk -v p="$SHEPHERD_PANE" '$1==p {print $2}' | head -1)

# No hacer nada si shepherd ya está en el window actual
[ "$SHEPHERD_WIN" = "$CURRENT_WIN" ] && exit 0

# Capturar pane actual para auto-selección al arrancar shepherd
tmux display-message -p '#{pane_id}' > "$HOME/.cache/shepherd-focus-pane"

# Guardar el layout actual del window destino ANTES de agregar shepherd
# (para poder restaurarlo cuando shepherd se vaya).
CURRENT_LAYOUT_FILE="$LAYOUT_DIR/${CURRENT_WIN}.layout"
tmux display-message -t "$CURRENT_WIN" -p '#{window_layout}' > "$CURRENT_LAYOUT_FILE"

# Matar shepherd en el window anterior (reflowa distorsionado)
tmux kill-pane -t "$SHEPHERD_PANE"

# Restaurar el layout del window anterior a como estaba ANTES de que shepherd
# llegara (deshace la distorsión acumulada por el reflow).
OLD_LAYOUT_FILE="$LAYOUT_DIR/${SHEPHERD_WIN}.layout"
if [ -f "$OLD_LAYOUT_FILE" ]; then
    tmux select-layout -t "$SHEPHERD_WIN" "$(cat "$OLD_LAYOUT_FILE")" 2>/dev/null || true
    rm -f "$OLD_LAYOUT_FILE"
fi

# Crear nuevo shepherd en el window actual (columna full-height a la derecha, 22%)
tmux split-window -h -f -l 22% -t "$CURRENT_WIN" -d "shepherd"
