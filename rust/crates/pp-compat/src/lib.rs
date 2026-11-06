#![allow(dead_code)]
mod logs;
pub use logs::{CoreLog, log_folder, recent_diagnostics, recent_logs};
pub mod redact;
#[cfg(windows)]
mod windows_security;
