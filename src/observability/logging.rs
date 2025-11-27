//! Logging module with structured logging, slow query logging, and dynamic log level adjustment
//!
//! Features:
//! - Structured logging with JSON format support
//! - Dynamic log level adjustment via CONFIG SET
//! - Slow query logging for performance monitoring
//! - Log rotation and archival support

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::Level;

/// Maximum number of slow queries to keep in memory
const DEFAULT_SLOWLOG_MAX_LEN: usize = 128;

/// Default slow query threshold in microseconds (10ms)
const DEFAULT_SLOWLOG_THRESHOLD_US: u64 = 10_000;

/// Log format type
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum LogFormat {
    /// Plain text format (default)
    #[default]
    Text,
    /// JSON structured format
    Json,
}

impl LogFormat {
    /// Parse log format from string
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "text" | "plain" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// Log configuration
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Log level (trace, debug, info, warn, error)
    pub level: Level,
    /// Log format (text or json)
    pub format: LogFormat,
    /// Log file path (None for stdout only)
    pub file_path: Option<String>,
    /// Enable console output
    pub console: bool,
    /// Max log file size before rotation (in bytes)
    pub max_size: u64,
    /// Number of rotated log files to keep
    pub max_backups: usize,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: Level::INFO,
            format: LogFormat::Text,
            file_path: None,
            console: true,
            max_size: 100 * 1024 * 1024, // 100MB
            max_backups: 10,
        }
    }
}

impl LogConfig {
    /// Parse log level from string
    pub fn parse_level(s: &str) -> Option<Level> {
        match s.to_lowercase().as_str() {
            "trace" => Some(Level::TRACE),
            "debug" => Some(Level::DEBUG),
            "info" => Some(Level::INFO),
            "warn" | "warning" => Some(Level::WARN),
            "error" => Some(Level::ERROR),
            _ => None,
        }
    }

    /// Convert level to string
    pub fn level_to_string(level: Level) -> &'static str {
        match level {
            Level::TRACE => "trace",
            Level::DEBUG => "debug",
            Level::INFO => "info",
            Level::WARN => "warn",
            Level::ERROR => "error",
        }
    }
}

/// Slow query entry
#[derive(Debug, Clone)]
pub struct SlowQueryEntry {
    /// Unique ID for this slow query
    pub id: u64,
    /// Unix timestamp when the query was logged
    pub timestamp: u64,
    /// Execution time in microseconds
    pub duration_us: u64,
    /// The command that was executed
    pub command: String,
    /// Command arguments
    pub args: Vec<String>,
    /// Client address (if available)
    pub client_addr: Option<String>,
}

impl SlowQueryEntry {
    /// Format as Redis-compatible array response
    pub fn to_resp_array(&self) -> Vec<(String, String)> {
        vec![
            ("id".to_string(), self.id.to_string()),
            ("timestamp".to_string(), self.timestamp.to_string()),
            ("duration_us".to_string(), self.duration_us.to_string()),
            ("command".to_string(), self.command.clone()),
            ("args".to_string(), self.args.join(" ")),
        ]
    }
}

/// Slow query log manager
#[derive(Debug)]
pub struct SlowQueryLog {
    /// Slow query entries (most recent first)
    entries: RwLock<VecDeque<SlowQueryEntry>>,
    /// Maximum number of entries to keep
    max_len: AtomicU64,
    /// Slow query threshold in microseconds
    threshold_us: AtomicU64,
    /// Next entry ID
    next_id: AtomicU64,
}

impl Default for SlowQueryLog {
    fn default() -> Self {
        Self::new()
    }
}

impl SlowQueryLog {
    /// Create a new slow query log
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(VecDeque::with_capacity(DEFAULT_SLOWLOG_MAX_LEN)),
            max_len: AtomicU64::new(DEFAULT_SLOWLOG_MAX_LEN as u64),
            threshold_us: AtomicU64::new(DEFAULT_SLOWLOG_THRESHOLD_US),
            next_id: AtomicU64::new(0),
        }
    }

    /// Create with custom settings
    pub fn with_settings(max_len: usize, threshold_us: u64) -> Self {
        Self {
            entries: RwLock::new(VecDeque::with_capacity(max_len)),
            max_len: AtomicU64::new(max_len as u64),
            threshold_us: AtomicU64::new(threshold_us),
            next_id: AtomicU64::new(0),
        }
    }

    /// Get the slow query threshold in microseconds
    pub fn threshold_us(&self) -> u64 {
        self.threshold_us.load(Ordering::Relaxed)
    }

    /// Set the slow query threshold in microseconds
    pub fn set_threshold_us(&self, threshold: u64) {
        self.threshold_us.store(threshold, Ordering::Relaxed);
    }

    /// Get the maximum log length
    pub fn max_len(&self) -> usize {
        self.max_len.load(Ordering::Relaxed) as usize
    }

    /// Set the maximum log length
    pub fn set_max_len(&self, max_len: usize) {
        self.max_len.store(max_len as u64, Ordering::Relaxed);
        // Trim if needed
        if let Ok(mut entries) = self.entries.write() {
            while entries.len() > max_len {
                entries.pop_back();
            }
        }
    }

    /// Record a slow query if it exceeds the threshold
    pub fn record(
        &self,
        command: &str,
        args: &[String],
        duration: Duration,
        client_addr: Option<String>,
    ) {
        let duration_us = duration.as_micros() as u64;
        if duration_us < self.threshold_us.load(Ordering::Relaxed) {
            return;
        }

        let entry = SlowQueryEntry {
            id: self.next_id.fetch_add(1, Ordering::SeqCst),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            duration_us,
            command: command.to_string(),
            args: args.to_vec(),
            client_addr,
        };

        if let Ok(mut entries) = self.entries.write() {
            entries.push_front(entry);
            let max_len = self.max_len.load(Ordering::Relaxed) as usize;
            while entries.len() > max_len {
                entries.pop_back();
            }
        }
    }

    /// Get recent slow queries
    pub fn get(&self, count: usize) -> Vec<SlowQueryEntry> {
        if let Ok(entries) = self.entries.read() {
            entries.iter().take(count).cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// Get the number of entries
    pub fn len(&self) -> usize {
        if let Ok(entries) = self.entries.read() {
            entries.len()
        } else {
            0
        }
    }

    /// Check if the log is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reset/clear the slow query log
    pub fn reset(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.clear();
        }
    }
}

/// Logging manager for dynamic log level adjustment and slow query logging
#[derive(Debug)]
pub struct LoggingManager {
    /// Current log level
    current_level: RwLock<Level>,
    /// Slow query log
    slow_query_log: Arc<SlowQueryLog>,
}

impl Default for LoggingManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LoggingManager {
    /// Create a new logging manager
    pub fn new() -> Self {
        Self {
            current_level: RwLock::new(Level::INFO),
            slow_query_log: Arc::new(SlowQueryLog::new()),
        }
    }

    /// Create with a specific log level
    pub fn with_level(level: Level) -> Self {
        Self {
            current_level: RwLock::new(level),
            slow_query_log: Arc::new(SlowQueryLog::new()),
        }
    }

    /// Get the current log level
    pub fn level(&self) -> Level {
        *self.current_level.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Set the log level dynamically
    pub fn set_level(&self, level: Level) {
        if let Ok(mut current) = self.current_level.write() {
            *current = level;
        }
    }

    /// Get the slow query log
    pub fn slow_query_log(&self) -> Arc<SlowQueryLog> {
        Arc::clone(&self.slow_query_log)
    }

    /// Record command execution time for potential slow query logging
    pub fn record_command(&self, command: &str, args: &[String], start: Instant) {
        let duration = start.elapsed();
        self.slow_query_log.record(command, args, duration, None);
    }

    /// Record command execution time with client address
    pub fn record_command_with_client(
        &self,
        command: &str,
        args: &[String],
        start: Instant,
        client_addr: Option<String>,
    ) {
        let duration = start.elapsed();
        self.slow_query_log
            .record(command, args, duration, client_addr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_format_parsing() {
        assert_eq!(LogFormat::parse("text"), Some(LogFormat::Text));
        assert_eq!(LogFormat::parse("json"), Some(LogFormat::Json));
        assert_eq!(LogFormat::parse("plain"), Some(LogFormat::Text));
        assert_eq!(LogFormat::parse("invalid"), None);
    }

    #[test]
    fn test_log_level_parsing() {
        assert_eq!(LogConfig::parse_level("trace"), Some(Level::TRACE));
        assert_eq!(LogConfig::parse_level("debug"), Some(Level::DEBUG));
        assert_eq!(LogConfig::parse_level("info"), Some(Level::INFO));
        assert_eq!(LogConfig::parse_level("warn"), Some(Level::WARN));
        assert_eq!(LogConfig::parse_level("error"), Some(Level::ERROR));
        assert_eq!(LogConfig::parse_level("invalid"), None);
    }

    #[test]
    fn test_slow_query_log() {
        let log = SlowQueryLog::with_settings(10, 1000); // 1ms threshold

        // Fast query should not be logged
        log.record(
            "GET",
            &["key".to_string()],
            Duration::from_micros(500),
            None,
        );
        assert!(log.is_empty());

        // Slow query should be logged
        log.record(
            "SET",
            &["key".to_string(), "value".to_string()],
            Duration::from_millis(2),
            None,
        );
        assert_eq!(log.len(), 1);

        let entries = log.get(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "SET");
    }

    #[test]
    fn test_slow_query_log_max_len() {
        let log = SlowQueryLog::with_settings(3, 0); // 0 threshold = log everything

        for i in 0..5 {
            log.record(&format!("CMD{}", i), &[], Duration::from_millis(1), None);
        }

        // Should only keep 3 entries
        assert_eq!(log.len(), 3);

        let entries = log.get(10);
        // Most recent first
        assert_eq!(entries[0].command, "CMD4");
        assert_eq!(entries[1].command, "CMD3");
        assert_eq!(entries[2].command, "CMD2");
    }

    #[test]
    fn test_slow_query_threshold_update() {
        let log = SlowQueryLog::new();

        // Default threshold
        assert_eq!(log.threshold_us(), DEFAULT_SLOWLOG_THRESHOLD_US);

        // Update threshold
        log.set_threshold_us(5000);
        assert_eq!(log.threshold_us(), 5000);
    }

    #[test]
    fn test_logging_manager() {
        let manager = LoggingManager::new();

        assert_eq!(manager.level(), Level::INFO);

        manager.set_level(Level::DEBUG);
        assert_eq!(manager.level(), Level::DEBUG);
    }
}
