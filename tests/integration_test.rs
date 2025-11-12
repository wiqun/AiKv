//! Integration tests for AiKv server
//!
//! These tests verify the end-to-end functionality of the AiKv server
//! by starting a test server and performing real RESP protocol operations.

use bytes::Bytes;
use redis::{Client, Commands, RedisResult};
use std::time::Duration;
use tokio::time::sleep;

const TEST_ADDR: &str = "redis://127.0.0.1:6380";

/// Helper function to start the test server in a background task
async fn start_test_server() {
    tokio::spawn(async {
        // Note: In a real scenario, you would start your aikv server here
        // For now, this is a placeholder for when the server is ready
        // aikv::run_server("127.0.0.1:6380").await.unwrap();
    });

    // Give the server time to start
    sleep(Duration::from_millis(100)).await;
}

/// Helper function to get a test Redis client
fn get_test_client() -> RedisResult<Client> {
    Client::open(TEST_ADDR)
}

#[tokio::test]
#[ignore] // Ignore until server can be started in tests
async fn test_basic_string_operations() {
    start_test_server().await;

    let client = get_test_client().expect("Failed to create client");
    let mut con = client.get_connection().expect("Failed to connect");

    // Test SET and GET
    let _: () = con.set("test_key", "test_value").expect("Failed to SET");
    let result: String = con.get("test_key").expect("Failed to GET");
    assert_eq!(result, "test_value");

    // Test DEL
    let deleted: i32 = con.del("test_key").expect("Failed to DEL");
    assert_eq!(deleted, 1);

    // Verify key is deleted
    let result: Option<String> = con.get("test_key").expect("Failed to GET after DEL");
    assert_eq!(result, None);
}

#[tokio::test]
#[ignore] // Ignore until server can be started in tests
async fn test_multiple_keys() {
    start_test_server().await;

    let client = get_test_client().expect("Failed to create client");
    let mut con = client.get_connection().expect("Failed to connect");

    // Test MSET
    let _: () = redis::cmd("MSET")
        .arg("key1")
        .arg("value1")
        .arg("key2")
        .arg("value2")
        .arg("key3")
        .arg("value3")
        .query(&mut con)
        .expect("Failed to MSET");

    // Test MGET
    let values: Vec<String> = redis::cmd("MGET")
        .arg("key1")
        .arg("key2")
        .arg("key3")
        .query(&mut con)
        .expect("Failed to MGET");

    assert_eq!(values, vec!["value1", "value2", "value3"]);
}

#[tokio::test]
#[ignore] // Ignore until server can be started in tests
async fn test_json_operations() {
    start_test_server().await;

    let client = get_test_client().expect("Failed to create client");
    let mut con = client.get_connection().expect("Failed to connect");

    // Test JSON.SET
    let json_data = r#"{"name":"Alice","age":30,"city":"NYC"}"#;
    let _: () = redis::cmd("JSON.SET")
        .arg("user:1")
        .arg("$")
        .arg(json_data)
        .query(&mut con)
        .expect("Failed to JSON.SET");

    // Test JSON.GET
    let result: String = redis::cmd("JSON.GET")
        .arg("user:1")
        .arg("$")
        .query(&mut con)
        .expect("Failed to JSON.GET");

    assert!(result.contains("Alice"));
    assert!(result.contains("NYC"));

    // Test JSON.TYPE
    let json_type: String = redis::cmd("JSON.TYPE")
        .arg("user:1")
        .arg("$.name")
        .query(&mut con)
        .expect("Failed to JSON.TYPE");

    assert_eq!(json_type, "string");
}

#[tokio::test]
#[ignore] // Ignore until server can be started in tests
async fn test_exists_command() {
    start_test_server().await;

    let client = get_test_client().expect("Failed to create client");
    let mut con = client.get_connection().expect("Failed to connect");

    // Set a key
    let _: () = con.set("exists_test", "value").expect("Failed to SET");

    // Test EXISTS
    let exists: i32 = con.exists("exists_test").expect("Failed to EXISTS");
    assert_eq!(exists, 1);

    // Test non-existent key
    let not_exists: i32 = con.exists("nonexistent").expect("Failed to EXISTS");
    assert_eq!(not_exists, 0);
}

#[tokio::test]
#[ignore] // Ignore until server can be started in tests
async fn test_append_command() {
    start_test_server().await;

    let client = get_test_client().expect("Failed to create client");
    let mut con = client.get_connection().expect("Failed to connect");

    // Set initial value
    let _: () = con.set("append_test", "Hello").expect("Failed to SET");

    // Append to value
    let new_len: i32 = redis::cmd("APPEND")
        .arg("append_test")
        .arg(" World")
        .query(&mut con)
        .expect("Failed to APPEND");

    assert_eq!(new_len, 11); // "Hello World" length

    // Verify appended value
    let result: String = con.get("append_test").expect("Failed to GET");
    assert_eq!(result, "Hello World");
}

#[tokio::test]
#[ignore] // Ignore until server can be started in tests
async fn test_ping_echo_commands() {
    start_test_server().await;

    let client = get_test_client().expect("Failed to create client");
    let mut con = client.get_connection().expect("Failed to connect");

    // Test PING
    let pong: String = redis::cmd("PING").query(&mut con).expect("Failed to PING");
    assert_eq!(pong, "PONG");

    // Test ECHO
    let echo: String = redis::cmd("ECHO")
        .arg("Hello AiKv")
        .query(&mut con)
        .expect("Failed to ECHO");
    assert_eq!(echo, "Hello AiKv");
}

#[tokio::test]
#[ignore] // Ignore until server can be started in tests
async fn test_concurrent_operations() {
    start_test_server().await;

    let client = get_test_client().expect("Failed to create client");

    // Spawn multiple concurrent tasks
    let mut handles = vec![];

    for i in 0..10 {
        let client = client.clone();
        let handle = tokio::spawn(async move {
            let mut con = client.get_connection().expect("Failed to connect");
            let key = format!("concurrent_key_{}", i);
            let value = format!("value_{}", i);

            // Perform operations
            let _: () = con.set(&key, &value).expect("Failed to SET");
            let result: String = con.get(&key).expect("Failed to GET");
            assert_eq!(result, value);
        });
        handles.push(handle);
    }

    // Wait for all tasks to complete
    for handle in handles {
        handle.await.expect("Task panicked");
    }
}

#[test]
fn test_resp_protocol_encoding() {
    // Test that RESP protocol types can be created and encoded
    // This doesn't require a server connection

    use aikv::protocol::types::RespValue;

    // Simple string
    let simple = RespValue::SimpleString("OK".to_string());
    let encoded = simple.serialize();
    assert_eq!(encoded, Bytes::from("+OK\r\n"));

    // Error
    let error = RespValue::Error("ERR unknown command".to_string());
    let encoded = error.serialize();
    assert_eq!(encoded, Bytes::from("-ERR unknown command\r\n"));

    // Integer
    let integer = RespValue::Integer(42);
    let encoded = integer.serialize();
    assert_eq!(encoded, Bytes::from(":42\r\n"));

    // Bulk string
    let bulk = RespValue::BulkString(Some(Bytes::from("hello")));
    let encoded = bulk.serialize();
    assert_eq!(encoded, Bytes::from("$5\r\nhello\r\n"));

    // Null bulk string
    let null_bulk = RespValue::BulkString(None);
    let encoded = null_bulk.serialize();
    assert_eq!(encoded, Bytes::from("$-1\r\n"));

    // Array
    let array = RespValue::Array(Some(vec![
        RespValue::SimpleString("OK".to_string()),
        RespValue::Integer(123),
    ]));
    let encoded = array.serialize();
    assert_eq!(encoded, Bytes::from("*2\r\n+OK\r\n:123\r\n"));
}
