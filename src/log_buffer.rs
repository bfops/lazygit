use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tracing::info;

#[derive(Debug, Clone)]
pub struct LogBuffer {
    inner: Arc<Mutex<LogState>>,
    capacity: usize,
}

#[derive(Debug)]
struct LogState {
    entries: VecDeque<String>,
    terminal_logging_enabled: bool,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LogState {
                entries: VecDeque::with_capacity(capacity),
                terminal_logging_enabled: false,
            })),
            capacity,
        }
    }

    pub fn record(&self, message: impl Into<String>) {
        let message = message.into();
        let should_log_to_terminal = self.push_entry(message.clone());
        if should_log_to_terminal {
            info!("{message}");
        }
    }

    pub fn record_stderr(&self, message: impl Into<String>) {
        let message = message.into();
        self.push_entry(message.clone());
        info!("{message}");
    }

    fn push_entry(&self, message: String) -> bool {
        let mut state = self.inner.lock().expect("log buffer poisoned");
        if state.entries.len() == self.capacity {
            state.entries.pop_front();
        }
        state.entries.push_back(message);
        state.terminal_logging_enabled
    }

    pub fn set_terminal_logging_enabled(&self, enabled: bool) {
        self.inner
            .lock()
            .expect("log buffer poisoned")
            .terminal_logging_enabled = enabled;
    }

    pub fn terminal_logging_enabled(&self) -> bool {
        self.inner
            .lock()
            .expect("log buffer poisoned")
            .terminal_logging_enabled
    }

    pub fn entries(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("log buffer poisoned")
            .entries
            .iter()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_keeps_latest_entries() {
        let logs = LogBuffer::new(2);
        logs.record("one");
        logs.record("two");
        logs.record("three");

        assert_eq!(logs.entries(), vec!["two", "three"]);
    }

    #[test]
    fn terminal_logging_flag_does_not_affect_entries() {
        let logs = LogBuffer::new(2);
        logs.set_terminal_logging_enabled(true);
        logs.record("one");
        logs.set_terminal_logging_enabled(false);
        logs.record("two");

        assert!(!logs.terminal_logging_enabled());
        assert_eq!(logs.entries(), vec!["one", "two"]);
    }
}
