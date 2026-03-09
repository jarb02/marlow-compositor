# Marlow Compositor — Instructions for Claude Code

## Architecture
- Rust compositor using Smithay, running on Fedora 43 laptop (Intel Iris Xe)
- Two backends: --winit (nested in Sway, for development) and KMS (TTY direct, for production)
- IPC with Python agent via Unix socket + MessagePack at $XDG_RUNTIME_DIR/marlow-compositor.sock
- Dual-seat: user seat (hardware) + marlow-agent seat (internal, invisible to apps)
- Shadow Mode: user_space (visible) + shadow_space (invisible, offscreen rendering)

## Session Startup (spawn_session_apps)
Order: mako -> swaybg -> waybar -> foot -> mic-setup.sh -> daemon (1s) -> voice daemon (3s) -> sidebar (5s) -> notification (6s).
Loads env vars from ~/.config/marlow/env. Voice daemon crash does not affect compositor.

## Development Rules
- Think through the problem before writing code. Understand root cause first.
- When a fix fails, add diagnostic logging BEFORE trying another fix
- Every change must work in BOTH winit and KMS backends
- Always cargo build --release after changes — test_kms.sh uses release build
- Pin Smithay to git rev, never follow master blindly

## Testing
- SSH from workstation: ssh josemarlow@192.168.5.107
- Winit test: cargo run -- --winit -c foot
- KMS test: Jose runs ./test_kms.sh from TTY (Ctrl+Alt+F3)
- Logs: /tmp/marlow-kms.log

## Git
- Email: jarb02@users.noreply.github.com
- Never include Co-authored-by or Claude references in commits
- Push to: git@github.com:jarb02/marlow-compositor.git (main branch)

## IPC Protocol (18 commands)
Core: Ping, ListWindows, GetWindowInfo, FocusWindow, GetSeatStatus, Subscribe
Input: SendKey, SendText, SendClick, SendHotkey (all target window_id via agent_seat)
Shadow: LaunchInShadow, GetShadowWindows, MoveToShadow, MoveToUser
Manage: CloseWindow, MinimizeWindow, MaximizeWindow
Screenshot: RequestScreenshot (supports shadow windows)

## Key Architecture
- Input routing: all input commands focus target window on agent_seat before sending input.
- Shadow screenshots use separate pending/buffer mechanism.
- manage_window: Close/Minimize/Maximize via xdg_toplevel.

## Waybar Status Indicator
custom/marlow-status module polls daemon /health + voice trigger file every 2s.
Colors: gray=idle, blue+pulse=listening, amber=processing, green=responding, red=error.
