# Marlow Compositor — Instructions for Claude Code

## Architecture
- Rust compositor using Smithay, running on Fedora 43 laptop (Intel Iris Xe)
- Two backends: --winit (nested in Sway, for development) and KMS (TTY direct, for production)
- IPC with Python agent via Unix socket + MessagePack at $XDG_RUNTIME_DIR/marlow-compositor.sock
- Dual-seat: user seat (hardware) + marlow-agent seat (internal, invisible to apps)
- Shadow Mode: user_space (visible) + shadow_space (invisible, offscreen rendering)

## Development Rules
- Think through the problem before writing code. Understand root cause first.
- When a fix fails, add diagnostic logging BEFORE trying another fix
- Every change must work in BOTH winit and KMS backends
- Test mentally: will this work on real hardware in TTY mode?
- Always cargo build --release after changes — test_kms.sh uses release build
- Pin Smithay to git rev, never follow master blindly

## Common Pitfalls
- Layer surfaces need initial configure via commit handler (geo=0x0 bug)
- KMS render loop needs timer fallback (VBlank chain dies on empty frames)
- Wayland protocols must be explicitly implemented (wp_viewporter, fractional_scale, etc.)
- Environment variables (WAYLAND_DISPLAY, XDG_RUNTIME_DIR) must be passed to spawned processes
- Keybinds fire on press AND release — check KeyState::Pressed only

## Testing
- SSH from workstation: ssh josemarlow@192.168.5.107
- Winit test: cargo run -- --winit -c foot
- KMS test: Jose runs ./test_kms.sh from TTY (Ctrl+Alt+F3)
- Logs: /tmp/marlow-kms.log
- Always leave desktop clean after tests (kill all spawned processes)

## Git
- Email: jarb02@users.noreply.github.com
- Never include Co-authored-by or Claude references in commits
- Push to: git@github.com:jarb02/marlow-compositor.git (main branch)

## Current Issue (March 7, 2026)
Shadow Mode goal "search for weather in homestead florida" reaches 1/2 steps.
- Step 1 (launch_in_shadow): May succeed (Firefox spawns) but move_to_user fails
- The daemon log and compositor log have the diagnostics
- Check: grep 'step\|shadow\|move_to_user\|fail\|error' ~/.marlow/daemon.log | tail -40
- Check: grep 'IPC request\|LaunchInShadow\|MoveToUser\|shadow' /tmp/marlow-kms.log | tail -20
- The prompt tells LLM to use 2-step plan: launch_in_shadow + move_to_user
- launch_in_shadow now splits command args correctly (415b16a)
- Scoring fixed: 0/N steps = FAILED not success (f6deff3)
- Key: launch_in_shadow waits up to 10s polling for window, returns window_id
- The window_id from step 1 must be passed to step 2 (move_to_user)
