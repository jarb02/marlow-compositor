# Auto-start Marlow OS on TTY3
# Add this to the END of ~/.bash_profile to enable:
#   source ~/marlow-compositor/bash_profile_snippet.sh
if [ "$(tty)" = "/dev/tty3" ] && [ -z "$WAYLAND_DISPLAY" ]; then
    exec ~/marlow-compositor/start-marlow-os.sh
fi
