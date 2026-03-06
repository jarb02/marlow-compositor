#!/bin/bash
# Test KMS backend — run this from a TTY (Ctrl+Alt+F3), NOT inside Sway.
# Press Ctrl+Q to exit the compositor.

cd ~/marlow-compositor || exit 1

echo "=== Marlow Compositor KMS Test ==="
echo "This test runs the compositor directly on DRM/KMS hardware."
echo "You should see a dark gray background."
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

# Clean up any stale socket
rm -f /run/user/$(id -u)/marlow-compositor.sock

echo "Starting compositor..."
echo "Log: /tmp/marlow-kms.log"
echo ""

# Run with -c foot to spawn a terminal
cargo run -- -c foot 2>&1 | tee /tmp/marlow-kms.log

echo ""
echo "Compositor exited. Log saved to /tmp/marlow-kms.log"
