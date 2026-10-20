/// Limits how many requests are admitted per window.
pub struct RateLimiter {
    remaining: u32,
}

impl RateLimiter {
    /// Take one permit from the limiter, or refuse when none remain.
    pub fn try_acquire(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }
}
