#!/bin/bash
# Marlow OS — start compositor on bare TTY
# Called from .bash_profile on TTY3 auto-login

export XDG_RUNTIME_DIR=/run/user/$(id -u)
export XDG_SESSION_TYPE=wayland

# Start compositor
exec ~/marlow-compositor/target/release/marlow-compositor
