use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct HostStats {
    pub input_events: AtomicU64,
    pub invalid_input_events: AtomicU64,
    pub frames_sent: AtomicU64,
}

impl HostStats {
    pub fn input_ok(&self) {
        self.input_events.fetch_add(1, Ordering::Relaxed);
    }
    pub fn input_invalid(&self) {
        self.invalid_input_events.fetch_add(1, Ordering::Relaxed);
    }
}
