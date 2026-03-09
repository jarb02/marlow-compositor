#!/bin/bash
# Marlow status indicator for waybar — polls daemon /status

STATUS=$(curl -s --max-time 2 http://localhost:8420/health 2>/dev/null | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    print(d.get('status', 'off'))
except:
    print('off')
" 2>/dev/null)

# Check voice trigger file for listening state
if [ -f /tmp/marlow-voice-trigger ]; then
    VOICE=$(cat /tmp/marlow-voice-trigger 2>/dev/null)
    if [ "$VOICE" = "press" ]; then
        STATUS="listening"
    fi
fi

case "$STATUS" in
    ok)
        echo '{"text": "●", "class": "idle", "tooltip": "Marlow: listo"}'
        ;;
    listening)
        echo '{"text": "●", "class": "listening", "tooltip": "Marlow: escuchando"}'
        ;;
    off)
        echo '{"text": "○", "class": "off", "tooltip": "Marlow: apagado"}'
        ;;
    *)
        echo '{"text": "●", "class": "idle", "tooltip": "Marlow: '"$STATUS"'"}'
        ;;
esac
