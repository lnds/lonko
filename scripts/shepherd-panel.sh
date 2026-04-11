#!/bin/bash
# Toggle shepherd side panel.
# Usage: shepherd-panel.sh [agents|sessions]
# Without args: toggle/focus shepherd.
# With args: open/focus shepherd and switch to the specified tab.

TAB_ARG="$1"
CURRENT_WIN="$(tmux display-message -p '#{window_id}')"

# Buscar shepherd en el window actual
SHEPHERD_PANE=$(tmux list-panes -F "#{pane_id} #{pane_current_command}" \
    | awk '$2 == "shepherd" {print $1}' | head -1)

if [ -n "$SHEPHERD_PANE" ]; then
    ACTIVE_PANE=$(tmux display-message -p '#{pane_id}')
    if [ "$ACTIVE_PANE" = "$SHEPHERD_PANE" ] && [ -z "$TAB_ARG" ]; then
        # Ya estamos en shepherd sin tab arg — volver al pane anterior
        tmux select-pane -l
    else
        # Panel visible — enfocar y enviar tecla de tab si corresponde
        tmux select-pane -t "$SHEPHERD_PANE"
        if [ "$TAB_ARG" = "agents" ]; then
            tmux send-keys -t "$SHEPHERD_PANE" "a"
        elif [ "$TAB_ARG" = "sessions" ]; then
            tmux send-keys -t "$SHEPHERD_PANE" "s"
        fi
    fi
else
    # Buscar shepherd en cualquier sesión (puede estar en shepherd-tray u otra)
    TRAY_PANE=$(tmux list-panes -aF "#{pane_id} #{pane_current_command} #{session_name}" \
        | awk '$2 == "shepherd" && $3 != ENVIRON["CURRENT_SESSION"] {print $1}' \
        | head -1)

    # Si no está fuera de esta sesión, buscar en cualquier lado (incluso shepherd-tray)
    if [ -z "$TRAY_PANE" ]; then
        TRAY_PANE=$(tmux list-panes -aF "#{pane_id} #{pane_current_command}" \
            | awk '$2 == "shepherd" {print $1}' | head -1)
    fi

    if [ -z "$TRAY_PANE" ]; then
        # No hay shepherd corriendo — arrancar en shepherd-tray
        tmux has-session -t shepherd-tray 2>/dev/null \
            || tmux new-session -d -s shepherd-tray
        tmux send-keys -t shepherd-tray: "shepherd" Enter
        # Wait for shepherd to start (may take >500ms on first launch)
        for _try in 1 2 3 4 5; do
            sleep 0.3
            TRAY_PANE=$(tmux list-panes -aF "#{pane_id} #{pane_current_command}" \
                | awk '$2 == "shepherd" {print $1}' | head -1)
            [ -n "$TRAY_PANE" ] && break
        done
    fi

    [ -z "$TRAY_PANE" ] && exit 1

    # Capturar pane actual para auto-selección de sesión en shepherd
    tmux display-message -p '#{pane_id}' > "${HOME}/.cache/shepherd-focus-pane"

    # Guardar el layout actual del window destino ANTES de agregar shepherd
    # (para poder restaurarlo cuando shepherd se vaya y evitar drift).
    LAYOUT_DIR="$HOME/.cache/shepherd-layouts"
    mkdir -p "$LAYOUT_DIR"
    tmux display-message -t "$CURRENT_WIN" -p '#{window_layout}' \
        > "$LAYOUT_DIR/${CURRENT_WIN}.layout"

    # Kill-and-respawn: matar el pane viejo (incluso si estaba en shepherd-tray)
    # y crear uno nuevo con split-window -h -f (full-height garantizado a la derecha).
    SHEPHERD_CMD="shepherd"
    [ -n "$TAB_ARG" ] && SHEPHERD_CMD="shepherd --tab $TAB_ARG"

    tmux kill-pane -t "$TRAY_PANE"
    tmux split-window -h -f -l 22% -t "$CURRENT_WIN" -d "$SHEPHERD_CMD"
fi
