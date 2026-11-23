//! Keeps the most recent log lines in memory for the settings window, while still writing to stderr.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

const MAX_LINES: usize = 500;

fn buffer() -> &'static Mutex<VecDeque<String>> {
    static BUF: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    BUF.get_or_init(|| Mutex::new(VecDeque::new()))
}

pub fn lines() -> Vec<String> {
    buffer()
        .lock()
        .map(|b| b.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn clear() {
    if let Ok(mut b) = buffer().lock() {
        b.clear();
    }
}

/// `env_logger` target that tees into the buffer.
pub struct Tee;

impl Write for Tee {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(data);
        if let Ok(mut b) = buffer().lock() {
            for line in String::from_utf8_lossy(data).lines() {
                if line.is_empty() {
                    continue;
                }
                if b.len() >= MAX_LINES {
                    b.pop_front();
                }
                b.push_back(line.to_string());
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}
