//! A spawned `katalogr serve` / `katalogr api` that cannot outlive its test (FEAT-126).
#![allow(dead_code)]

use std::process::Child;

/// Owns a spawned server. `Drop` kills and reaps it, so a panic before the explicit end of a
/// test cannot leak a process (and the file descriptors it inherited).
pub struct ServerGuard {
    child: Child,
}

impl ServerGuard {
    pub fn new(child: Child) -> Self {
        Self { child }
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
