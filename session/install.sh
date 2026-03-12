#\!/bin/bash
# Install Marlow OS session files
# Run with: sudo bash session/install.sh (for system files) + bash session/install.sh --user (for user services)
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

if [ "$1" = "--user" ]; then
    echo "Installing systemd user services..."
    mkdir -p ~/.config/systemd/user
    cp "$SCRIPT_DIR"/marlow-session.target ~/.config/systemd/user/
    cp "$SCRIPT_DIR"/marlow-mako.service ~/.config/systemd/user/
    cp "$SCRIPT_DIR"/marlow-swaybg.service ~/.config/systemd/user/
    cp "$SCRIPT_DIR"/marlow-waybar.service ~/.config/systemd/user/
    cp "$SCRIPT_DIR"/marlow-daemon.service ~/.config/systemd/user/
    cp "$SCRIPT_DIR"/marlow-voice.service ~/.config/systemd/user/
    cp "$SCRIPT_DIR"/marlow-sidebar.service ~/.config/systemd/user/
    systemctl --user daemon-reload
    echo "Done. Verify: systemctl --user list-unit-files | grep marlow"
else
    echo "Installing system-wide session files (requires sudo)..."
    sudo install -m 755 "$SCRIPT_DIR/start-marlow" /usr/local/bin/start-marlow
    sudo install -m 644 "$SCRIPT_DIR/marlow.desktop" /usr/share/wayland-sessions/marlow.desktop
    echo "Done. Verify: ls /usr/share/wayland-sessions/"
    echo ""
    echo "Now run: bash session/install.sh --user"
fi
