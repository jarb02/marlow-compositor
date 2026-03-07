#!/bin/bash
# Test KMS backend — run this from a TTY (Ctrl+Alt+F3), NOT inside Sway.
# Press Ctrl+Q to exit the compositor.

cd ~/marlow-compositor || exit 1

echo "=== Marlow Compositor KMS Test ==="
echo "This test runs the compositor directly on DRM/KMS hardware."
echo "You should see a dark gray background with a white cursor."
echo "Move the touchpad/mouse — cursor should follow."
echo "Press Ctrl+Q to exit."
echo ""

# Check if we're in a TTY (no WAYLAND_DISPLAY or DISPLAY)
if [ -n "$WAYLAND_DISPLAY" ] || [ -n "$DISPLAY" ]; then
    echo "WARNING: You appear to be running inside a graphical session."
    echo "For KMS mode, switch to a TTY first: Ctrl+Alt+F3"
    echo "Then run: ./test_kms.sh"
    echo ""
    read -p "Continue anyway? (y/N) " confirm
    [ "$confirm" != "y" ] && exit 0
fi

# Ensure seatd/logind is running
if ! command -v seatd &>/dev/null && ! systemctl is-active systemd-logind &>/dev/null; then
    echo "ERROR: Neither seatd nor systemd-logind is running."
    echo "Install and start seatd: sudo dnf install seatd && sudo systemctl start seatd"
    exit 1
fi

# Clean up leftover processes from previous tests
echo "Cleaning up stale processes..."
pkill -9 firefox 2>/dev/null
pkill -f daemon_linux 2>/dev/null
rm -f ~/.mozilla/firefox/*/lock ~/.mozilla/firefox/*/.parentlock 2>/dev/null
sleep 1

# Clean up any stale socket
rm -f /run/user/$(id -u)/marlow-compositor.sock

LOG=/tmp/marlow-kms.log
echo "Starting compositor..."
echo "Log: $LOG"
echo ""

# Run release build with foot terminal, log everything
RUST_LOG=info cargo run --release -- -c foot 2>&1 | tee "$LOG"

echo ""
echo "=== Compositor exited ==="
echo ""
echo "--- Last 30 lines of log ---"
tail -30 "$LOG"
echo ""
echo "--- Input events ---"
grep "Input:" "$LOG" | head -20
echo ""
echo "--- Render frames ---"
grep "Render frame" "$LOG" | head -10
echo ""
echo "--- Window mapping ---"
grep "Window.*mapped" "$LOG"
echo ""
echo "--- Errors/Warnings ---"
grep -i "error\|warn\|failed" "$LOG" | grep -v "GL Extensions"
echo ""
echo "Full log saved to $LOG"
