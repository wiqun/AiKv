//! Tests for the MONITOR command functionality
//!
//! The MONITOR command streams all commands processed by the server in real-time.
//! These tests verify the MonitorBroadcaster and MonitorMessage functionality.

use aikv::server::monitor::{MonitorBroadcaster, MonitorMessage};
use std::sync::Arc;

/// Unix timestamp for 2000-01-01 00:00:00 UTC
const YEAR_2000_UNIX_TIMESTAMP: f64 = 946684800.0;

#[test]
fn test_monitor_message_format_basic() {
    let msg = MonitorMessage::new(
        0,
        "127.0.0.1:60866".to_string(),
        "SET".to_string(),
        vec!["key".to_string(), "value".to_string()],
    );

    let formatted = msg.format();

    // Check format: timestamp [db client] "command" "arg1" "arg2"
    assert!(formatted.contains("[0 127.0.0.1:60866]"));
    assert!(formatted.contains("\"SET\""));
    assert!(formatted.contains("\"key\""));
    assert!(formatted.contains("\"value\""));
}

#[test]
fn test_monitor_message_format_no_args() {
    let msg = MonitorMessage::new(0, "127.0.0.1:60866".to_string(), "PING".to_string(), vec![]);

    let formatted = msg.format();

    assert!(formatted.contains("\"PING\""));
    assert!(formatted.ends_with("\"PING\""));
}

#[test]
fn test_monitor_message_format_special_chars() {
    let msg = MonitorMessage::new(
        0,
        "127.0.0.1:60866".to_string(),
        "SET".to_string(),
        vec![
            "key".to_string(),
            "value with \"quotes\" and\nnewline".to_string(),
        ],
    );

    let formatted = msg.format();

    // Special characters should be escaped
    assert!(formatted.contains("\\\"quotes\\\""));
    assert!(formatted.contains("\\n"));
}

#[test]
fn test_monitor_message_different_databases() {
    let msg_db0 = MonitorMessage::new(
        0,
        "127.0.0.1:60866".to_string(),
        "GET".to_string(),
        vec!["key".to_string()],
    );

    let msg_db1 = MonitorMessage::new(
        1,
        "127.0.0.1:60866".to_string(),
        "GET".to_string(),
        vec!["key".to_string()],
    );

    assert!(msg_db0.format().contains("[0 127.0.0.1:60866]"));
    assert!(msg_db1.format().contains("[1 127.0.0.1:60866]"));
}

#[tokio::test]
async fn test_monitor_broadcaster_register_unregister() {
    let broadcaster = MonitorBroadcaster::new();

    // Initially no monitors
    assert!(!broadcaster.has_monitors());
    assert_eq!(broadcaster.monitor_count(), 0);

    // Register first monitor
    broadcaster
        .register_monitor(1, "127.0.0.1:12345".to_string())
        .await;
    assert!(broadcaster.has_monitors());
    assert_eq!(broadcaster.monitor_count(), 1);

    // Register second monitor
    broadcaster
        .register_monitor(2, "127.0.0.1:12346".to_string())
        .await;
    assert_eq!(broadcaster.monitor_count(), 2);

    // Unregister first monitor
    broadcaster.unregister_monitor(1).await;
    assert!(broadcaster.has_monitors());
    assert_eq!(broadcaster.monitor_count(), 1);

    // Unregister second monitor
    broadcaster.unregister_monitor(2).await;
    assert!(!broadcaster.has_monitors());
    assert_eq!(broadcaster.monitor_count(), 0);
}

#[tokio::test]
async fn test_monitor_broadcaster_double_unregister() {
    let broadcaster = MonitorBroadcaster::new();

    broadcaster
        .register_monitor(1, "127.0.0.1:12345".to_string())
        .await;
    assert_eq!(broadcaster.monitor_count(), 1);

    // Unregister
    broadcaster.unregister_monitor(1).await;
    assert_eq!(broadcaster.monitor_count(), 0);

    // Double unregister should be safe
    broadcaster.unregister_monitor(1).await;
    assert_eq!(broadcaster.monitor_count(), 0);
}

#[tokio::test]
async fn test_monitor_broadcast_message() {
    let broadcaster = Arc::new(MonitorBroadcaster::new());

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
        vec!["mykey".to_string(), "myvalue".to_string()],
    );

    let receivers_count = broadcaster.broadcast(msg);
    assert!(receivers_count >= 1);

    // Receive the message
    let received = receiver.recv().await.unwrap();
    assert_eq!(received.command, "SET");
    assert_eq!(received.args, vec!["mykey", "myvalue"]);
    assert_eq!(received.db, 0);
    assert_eq!(received.client_addr, "127.0.0.1:60866");
}

#[tokio::test]
async fn test_monitor_broadcast_command() {
    let broadcaster = Arc::new(MonitorBroadcaster::new());

    // Subscribe before registering
    let mut receiver = broadcaster.subscribe();
    broadcaster
        .register_monitor(1, "127.0.0.1:12345".to_string())
        .await;

    // Use the convenience method
    let receivers_count =
        broadcaster.broadcast_command(0, "127.0.0.1:60866", "GET", &["mykey".to_string()]);
    assert!(receivers_count >= 1);

    // Receive the message
    let received = receiver.recv().await.unwrap();
    assert_eq!(received.command, "GET");
    assert_eq!(received.args, vec!["mykey"]);
}

#[tokio::test]
async fn test_monitor_broadcast_no_monitors() {
    let broadcaster = MonitorBroadcaster::new();

    // No monitors registered, broadcast should return 0
    let receivers_count =
        broadcaster.broadcast_command(0, "127.0.0.1:60866", "GET", &["mykey".to_string()]);
    assert_eq!(receivers_count, 0);
}

#[tokio::test]
async fn test_monitor_multiple_receivers() {
    let broadcaster = Arc::new(MonitorBroadcaster::new());

    // Create multiple receivers
    let mut receiver1 = broadcaster.subscribe();
    let mut receiver2 = broadcaster.subscribe();

    broadcaster
        .register_monitor(1, "127.0.0.1:12345".to_string())
        .await;
    broadcaster
        .register_monitor(2, "127.0.0.1:12346".to_string())
        .await;

    // Broadcast a message
    broadcaster.broadcast_command(
        0,
        "127.0.0.1:60866",
        "SET",
        &["key".to_string(), "value".to_string()],
    );

    // Both receivers should get the message
    let received1 = receiver1.recv().await.unwrap();
    let received2 = receiver2.recv().await.unwrap();

    assert_eq!(received1.command, "SET");
    assert_eq!(received2.command, "SET");
}

#[test]
fn test_monitor_message_timestamp() {
    let msg = MonitorMessage::new(0, "127.0.0.1:60866".to_string(), "PING".to_string(), vec![]);

    // Timestamp should be a reasonable Unix timestamp (after year 2000)
    assert!(msg.timestamp > YEAR_2000_UNIX_TIMESTAMP);

    // Formatted message should contain the timestamp
    let formatted = msg.format();
    assert!(formatted.contains(&format!("{:.6}", msg.timestamp)));
}
