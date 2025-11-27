//! MONITOR command support for real-time command streaming
//!
//! This module implements Redis MONITOR functionality which allows clients
//! to see all commands processed by the server in real-time. This is useful
//! for debugging and profiling, and is supported by Redis desktop clients.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use tokio::sync::RwLock;
use tracing::debug;

/// A monitor message containing command details
#[derive(Clone, Debug)]
pub struct MonitorMessage {
    /// Unix timestamp with microseconds
    pub timestamp: f64,
    /// Database number
    pub db: usize,
    /// Client address
    pub client_addr: String,
    /// Command name
    pub command: String,
    /// Command arguments
    pub args: Vec<String>,
}

impl MonitorMessage {
    /// Create a new monitor message
    pub fn new(db: usize, client_addr: String, command: String, args: Vec<String>) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        Self {
            timestamp,
            db,
            client_addr,
            command,
            args,
        }
    }

    /// Format the message in Redis MONITOR format
    /// Example: 1339518083.107412 [0 127.0.0.1:60866] "SET" "foo" "bar"
    pub fn format(&self) -> String {
        let args_formatted: Vec<String> = self
            .args
            .iter()
            .map(|arg| format!("\"{}\"", Self::escape_arg(arg)))
            .collect();

        let args_str = if args_formatted.is_empty() {
            String::new()
        } else {
            format!(" {}", args_formatted.join(" "))
        };

        format!(
            "{:.6} [{} {}] \"{}\"{}",
            self.timestamp,
            self.db,
            self.client_addr,
            Self::escape_arg(&self.command),
            args_str
        )
    }

    /// Escape special characters in argument for display
    fn escape_arg(arg: &str) -> String {
        arg.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    }
}

/// Monitor broadcaster for sending commands to all monitoring clients
pub struct MonitorBroadcaster {
    /// Broadcast channel sender
    sender: broadcast::Sender<MonitorMessage>,
    /// Number of active monitors
    monitor_count: AtomicU64,
    /// Monitor client info (client_id -> client_addr)
    monitors: Arc<RwLock<HashMap<usize, String>>>,
}

impl MonitorBroadcaster {
    /// Create a new monitor broadcaster
    pub fn new() -> Self {
        // Channel capacity for monitor messages
        let (sender, _) = broadcast::channel(1024);
        Self {
            sender,
            monitor_count: AtomicU64::new(0),
            monitors: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Subscribe to monitor messages
    pub fn subscribe(&self) -> broadcast::Receiver<MonitorMessage> {
        self.sender.subscribe()
    }

    /// Register a new monitor client
    pub async fn register_monitor(&self, client_id: usize, client_addr: String) {
        let mut monitors = self.monitors.write().await;
        monitors.insert(client_id, client_addr);
        self.monitor_count.fetch_add(1, Ordering::SeqCst);
    }

    /// Unregister a monitor client
    pub async fn unregister_monitor(&self, client_id: usize) {
        let mut monitors = self.monitors.write().await;
        if monitors.remove(&client_id).is_some() {
            self.monitor_count.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Check if there are any active monitors
    pub fn has_monitors(&self) -> bool {
        self.monitor_count.load(Ordering::SeqCst) > 0
    }

    /// Get the number of active monitors
    pub fn monitor_count(&self) -> u64 {
        self.monitor_count.load(Ordering::SeqCst)
    }

    /// Broadcast a command to all monitors
    /// Returns the number of receivers that received the message
    pub fn broadcast(&self, message: MonitorMessage) -> usize {
        // Only send if there are active monitors (optimization)
        if !self.has_monitors() {
            return 0;
        }

        match self.sender.send(message) {
            Ok(count) => count,
            Err(e) => {
                // This can happen if all receivers have been dropped.
                // Log at debug level since this is expected during cleanup.
                debug!("Monitor broadcast failed (no active receivers): {}", e);
                0
            }
        }
    }

    /// Broadcast a command with the given details
    pub fn broadcast_command(
        &self,
        db: usize,
        client_addr: &str,
        command: &str,
        args: &[String],
    ) -> usize {
        if !self.has_monitors() {
            return 0;
        }

        let message = MonitorMessage::new(
            db,
            client_addr.to_string(),
            command.to_string(),
            args.to_vec(),
        );
        self.broadcast(message)
    }
}

impl Default for MonitorBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monitor_message_format() {
        let msg = MonitorMessage {
            timestamp: 1339518083.107412,
            db: 0,
            client_addr: "127.0.0.1:60866".to_string(),
            command: "SET".to_string(),
            args: vec!["foo".to_string(), "bar".to_string()],
        };

        let formatted = msg.format();
        assert!(formatted.contains("1339518083.107412"));
        assert!(formatted.contains("[0 127.0.0.1:60866]"));
        assert!(formatted.contains("\"SET\""));
        assert!(formatted.contains("\"foo\""));
        assert!(formatted.contains("\"bar\""));
    }

    #[test]
    fn test_monitor_message_escape() {
        let msg = MonitorMessage {
            timestamp: 1339518083.0,
            db: 0,
            client_addr: "127.0.0.1:60866".to_string(),
            command: "SET".to_string(),
            args: vec!["key".to_string(), "value with \"quotes\"".to_string()],
        };

        let formatted = msg.format();
        assert!(formatted.contains("\\\"quotes\\\""));
    }

    #[test]
    fn test_monitor_message_no_args() {
        let msg = MonitorMessage {
            timestamp: 1339518083.0,
            db: 0,
            client_addr: "127.0.0.1:60866".to_string(),
            command: "PING".to_string(),
            args: vec![],
        };

        let formatted = msg.format();
        assert!(formatted.ends_with("\"PING\""));
    }

    #[tokio::test]
    async fn test_monitor_broadcaster() {
        let broadcaster = MonitorBroadcaster::new();

        // Initially no monitors
        assert!(!broadcaster.has_monitors());
        assert_eq!(broadcaster.monitor_count(), 0);

        // Register a monitor
        broadcaster
            .register_monitor(1, "127.0.0.1:12345".to_string())
            .await;
        assert!(broadcaster.has_monitors());
        assert_eq!(broadcaster.monitor_count(), 1);

        // Unregister
        broadcaster.unregister_monitor(1).await;
        assert!(!broadcaster.has_monitors());
        assert_eq!(broadcaster.monitor_count(), 0);
    }

    #[tokio::test]
    async fn test_monitor_broadcast() {
        let broadcaster = MonitorBroadcaster::new();

        // Subscribe before registering
        let mut receiver = broadcaster.subscribe();
        broadcaster
            .register_monitor(1, "127.0.0.1:12345".to_string())
            .await;

        // Broadcast a message
        let msg = MonitorMessage::new(
            0,
            "127.0.0.1:60866".to_string(),
            "SET".to_string(),
            vec!["key".to_string(), "value".to_string()],
        );
        broadcaster.broadcast(msg.clone());

        // Receive the message
        let received = receiver.recv().await.unwrap();
        assert_eq!(received.command, "SET");
        assert_eq!(received.args, vec!["key", "value"]);
    }
}
