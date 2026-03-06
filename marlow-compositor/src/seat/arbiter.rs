use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

/// Arbitrates conflicts when user and agent operate the same window.
///
/// Rules:
/// - User always wins: if user clicks/types on agent's window, agent loses focus
/// - Pointer hover does NOT trigger conflict (grace period)
/// - Only keyboard input or button press from the user triggers conflict
pub struct SeatArbiter {
    /// Grace period in ms before declaring conflict on pointer hover (unused for now —
    /// conflicts are only triggered by active input, not hover)
    pub grace_ms: u64,
}

impl SeatArbiter {
    pub fn new() -> Self {
        Self { grace_ms: 200 }
    }

    /// Check if user input on `user_surface` conflicts with the agent's focus.
    /// Returns the conflicting window_id if both seats target the same surface.
    pub fn check_conflict(
        &self,
        user_surface: &WlSurface,
        agent_focus: Option<&WlSurface>,
    ) -> bool {
        match agent_focus {
            Some(agent_surface) => *agent_surface == *user_surface,
            None => false,
        }
    }
}
