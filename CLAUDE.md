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
