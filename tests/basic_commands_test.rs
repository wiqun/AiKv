use aikv::command::CommandExecutor;
use aikv::protocol::RespValue;
use aikv::storage::StorageAdapter;
use bytes::Bytes;

#[test]
fn test_database_commands() {
    let storage = StorageAdapter::new();
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Test SELECT
    let result = executor
        .execute("SELECT", &[Bytes::from("1")], &mut current_db, client_id)
        .unwrap();
    assert_eq!(result, RespValue::ok());
    assert_eq!(current_db, 1);

    // Test SET in database 1
    executor
        .execute(
            "SET",
            &[Bytes::from("key1"), Bytes::from("value1")],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // Test DBSIZE
    let result = executor
        .execute("DBSIZE", &[], &mut current_db, client_id)
        .unwrap();
    assert_eq!(result, RespValue::integer(1));

    // Test SELECT back to database 0
    executor
        .execute("SELECT", &[Bytes::from("0")], &mut current_db, client_id)
        .unwrap();
    assert_eq!(current_db, 0);

    // Database 0 should be empty
    let result = executor
        .execute("DBSIZE", &[], &mut current_db, client_id)
        .unwrap();
    assert_eq!(result, RespValue::integer(0));

    // Test MOVE command
    executor
        .execute("SELECT", &[Bytes::from("1")], &mut current_db, client_id)
        .unwrap();
    let result = executor
        .execute(
            "MOVE",
            &[Bytes::from("key1"), Bytes::from("0")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::integer(1));

    // Now database 1 should be empty
    let result = executor
        .execute("DBSIZE", &[], &mut current_db, client_id)
        .unwrap();
    assert_eq!(result, RespValue::integer(0));

    // And database 0 should have one key
    executor
        .execute("SELECT", &[Bytes::from("0")], &mut current_db, client_id)
        .unwrap();
    let result = executor
        .execute("DBSIZE", &[], &mut current_db, client_id)
        .unwrap();
    assert_eq!(result, RespValue::integer(1));

    // Test FLUSHDB
    let result = executor
        .execute("FLUSHDB", &[], &mut current_db, client_id)
        .unwrap();
    assert_eq!(result, RespValue::ok());
    let result = executor
        .execute("DBSIZE", &[], &mut current_db, client_id)
        .unwrap();
    assert_eq!(result, RespValue::integer(0));
}

#[test]
fn test_key_commands() {
    let storage = StorageAdapter::new();
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Set up test data
    executor
        .execute(
            "MSET",
            &[
                Bytes::from("user:1"),
                Bytes::from("Alice"),
                Bytes::from("user:2"),
                Bytes::from("Bob"),
                Bytes::from("product:1"),
                Bytes::from("Widget"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // Test KEYS
    let result = executor
        .execute("KEYS", &[Bytes::from("*")], &mut current_db, client_id)
        .unwrap();
    if let RespValue::Array(Some(keys)) = result {
        assert_eq!(keys.len(), 3);
    } else {
        panic!("Expected array");
    }

    // Test KEYS with pattern
    let result = executor
        .execute("KEYS", &[Bytes::from("user:*")], &mut current_db, client_id)
        .unwrap();
    if let RespValue::Array(Some(keys)) = result {
        assert_eq!(keys.len(), 2);
    } else {
        panic!("Expected array");
    }

    // Test SCAN - basic iteration
    let result = executor
        .execute("SCAN", &[Bytes::from("0")], &mut current_db, client_id)
        .unwrap();
    if let RespValue::Array(Some(scan_result)) = result {
        assert_eq!(scan_result.len(), 2); // [cursor, keys]
                                          // Check cursor (first element)
        assert!(matches!(&scan_result[0], RespValue::BulkString(Some(_))));
        // Check keys array (second element)
        assert!(matches!(&scan_result[1], RespValue::Array(Some(_))));
    } else {
        panic!("Expected array for SCAN result");
    }

    // Test SCAN with MATCH pattern
    let result = executor
        .execute(
            "SCAN",
            &[
                Bytes::from("0"),
                Bytes::from("MATCH"),
                Bytes::from("user:*"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();
    if let RespValue::Array(Some(scan_result)) = result {
        if let RespValue::Array(Some(keys)) = &scan_result[1] {
            // Should only return keys matching user:* pattern
            assert!(keys.len() <= 2);
        } else {
            panic!("Expected keys array");
        }
    } else {
        panic!("Expected array for SCAN result");
    }

    // Test SCAN with COUNT
    let result = executor
        .execute(
            "SCAN",
            &[Bytes::from("0"), Bytes::from("COUNT"), Bytes::from("1")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    if let RespValue::Array(Some(scan_result)) = result {
        if let RespValue::Array(Some(keys)) = &scan_result[1] {
            // Should return at most 1 key
            assert!(keys.len() <= 1);
        } else {
            panic!("Expected keys array");
        }
    } else {
        panic!("Expected array for SCAN result");
    }

    // Test RANDOMKEY
    let result = executor
        .execute("RANDOMKEY", &[], &mut current_db, client_id)
        .unwrap();
    assert!(matches!(result, RespValue::BulkString(Some(_))));

    // Test RENAME
    let result = executor
        .execute(
            "RENAME",
            &[Bytes::from("user:1"), Bytes::from("user:100")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::ok());

    // Verify old key doesn't exist
    let result = executor
        .execute(
            "EXISTS",
            &[Bytes::from("user:1")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::integer(0));

    // Verify new key exists
    let result = executor
        .execute(
            "EXISTS",
            &[Bytes::from("user:100")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::integer(1));

    // Test RENAMENX
    let result = executor
        .execute(
            "RENAMENX",
            &[Bytes::from("user:100"), Bytes::from("user:2")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::integer(0)); // user:2 already exists

    // Test TYPE
    let result = executor
        .execute("TYPE", &[Bytes::from("user:2")], &mut current_db, client_id)
        .unwrap();
    assert_eq!(result, RespValue::simple_string("string"));

    // Test COPY
    let result = executor
        .execute(
            "COPY",
            &[Bytes::from("user:2"), Bytes::from("user:2:backup")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::integer(1));

    // Verify copy exists
    let result = executor
        .execute(
            "GET",
            &[Bytes::from("user:2:backup")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::bulk_string("Bob"));
}

#[test]
fn test_expiration_commands() {
    let storage = StorageAdapter::new();
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Set up test data
    executor
        .execute(
            "SET",
            &[Bytes::from("key1"), Bytes::from("value1")],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // Test EXPIRE
    let result = executor
        .execute(
            "EXPIRE",
            &[Bytes::from("key1"), Bytes::from("100")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::integer(1));

    // Test TTL
    let result = executor
        .execute("TTL", &[Bytes::from("key1")], &mut current_db, client_id)
        .unwrap();
    if let RespValue::Integer(ttl) = result {
        assert!(ttl > 0 && ttl <= 100);
    } else {
        panic!("Expected integer TTL");
    }

    // Test PTTL
    let result = executor
        .execute("PTTL", &[Bytes::from("key1")], &mut current_db, client_id)
        .unwrap();
    if let RespValue::Integer(pttl) = result {
        assert!(pttl > 0 && pttl <= 100000);
    } else {
        panic!("Expected integer PTTL");
    }

    // Test PERSIST
    let result = executor
        .execute(
            "PERSIST",
            &[Bytes::from("key1")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::integer(1));

    // TTL should now be -1
    let result = executor
        .execute("TTL", &[Bytes::from("key1")], &mut current_db, client_id)
        .unwrap();
    assert_eq!(result, RespValue::integer(-1));

    // Test PEXPIRE
    let result = executor
        .execute(
            "PEXPIRE",
            &[Bytes::from("key1"), Bytes::from("50000")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::integer(1));

    // Test EXPIRETIME
    let result = executor
        .execute(
            "EXPIRETIME",
            &[Bytes::from("key1")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    if let RespValue::Integer(timestamp) = result {
        assert!(timestamp > 0);
    } else {
        panic!("Expected integer timestamp");
    }

    // Test PEXPIRETIME
    let result = executor
        .execute(
            "PEXPIRETIME",
            &[Bytes::from("key1")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    if let RespValue::Integer(timestamp) = result {
        assert!(timestamp > 0);
    } else {
        panic!("Expected integer timestamp");
    }

    // Test TTL on non-existent key
    let result = executor
        .execute(
            "TTL",
            &[Bytes::from("nonexistent")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::integer(-2));
}

#[test]
fn test_ping_command() {
    let storage = StorageAdapter::new();
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Test PING without argument - should return simple string "PONG"
    let result = executor
        .execute("PING", &[], &mut current_db, client_id)
        .unwrap();
    assert_eq!(result, RespValue::simple_string("PONG"));

    // Test PING with message argument - should return bulk string with the message
    let result = executor
        .execute("PING", &[Bytes::from("hello")], &mut current_db, client_id)
        .unwrap();
    assert_eq!(result, RespValue::bulk_string("hello"));

    // Test PING with empty string argument
    let result = executor
        .execute("PING", &[Bytes::from("")], &mut current_db, client_id)
        .unwrap();
    assert_eq!(result, RespValue::bulk_string(""));

    // Test PING with special characters
    let result = executor
        .execute(
            "PING",
            &[Bytes::from("hello world!")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::bulk_string("hello world!"));

    // Test PING with too many arguments - should return error
    let result = executor.execute(
        "PING",
        &[Bytes::from("hello"), Bytes::from("world")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_err());
}

#[test]
fn test_server_commands() {
    let storage = StorageAdapter::new();
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Register the client first (simulating what Connection does)
    executor
        .server_commands()
        .register_client(client_id, "127.0.0.1:12345".to_string())
        .unwrap();

    // Test INFO
    let result = executor
        .execute("INFO", &[], &mut current_db, client_id)
        .unwrap();
    assert!(matches!(result, RespValue::BulkString(Some(_))));

    // Test CONFIG GET
    let result = executor
        .execute(
            "CONFIG",
            &[Bytes::from("GET"), Bytes::from("server")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    if let RespValue::Array(Some(arr)) = result {
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], RespValue::bulk_string("server"));
        assert_eq!(arr[1], RespValue::bulk_string("aikv"));
    } else {
        panic!("Expected array");
    }

    // Test TIME
    let result = executor
        .execute("TIME", &[], &mut current_db, client_id)
        .unwrap();
    if let RespValue::Array(Some(arr)) = result {
        assert_eq!(arr.len(), 2);
        assert!(matches!(&arr[0], RespValue::BulkString(Some(_))));
        assert!(matches!(&arr[1], RespValue::BulkString(Some(_))));
    } else {
        panic!("Expected array");
    }

    // Test CLIENT SETNAME
    let result = executor
        .execute(
            "CLIENT",
            &[Bytes::from("SETNAME"), Bytes::from("test-client")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::ok());

    // Test CLIENT GETNAME
    let result = executor
        .execute(
            "CLIENT",
            &[Bytes::from("GETNAME")],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::bulk_string("test-client"));

    // Test CLIENT LIST
    let result = executor
        .execute("CLIENT", &[Bytes::from("LIST")], &mut current_db, client_id)
        .unwrap();
    assert!(matches!(result, RespValue::BulkString(Some(_))));
}

#[test]
fn test_scan_iteration() {
    let storage = StorageAdapter::new();
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Set up test data with many keys
    for i in 0..25 {
        executor
            .execute(
                "SET",
                &[
                    Bytes::from(format!("key:{}", i)),
                    Bytes::from(format!("value:{}", i)),
                ],
                &mut current_db,
                client_id,
            )
            .unwrap();
    }

    // Test full iteration with SCAN
    let mut cursor = 0;
    let mut all_keys = Vec::new();
    let mut iterations = 0;

    loop {
        let result = executor
            .execute(
                "SCAN",
                &[
                    Bytes::from(cursor.to_string()),
                    Bytes::from("COUNT"),
                    Bytes::from("5"),
                ],
                &mut current_db,
                client_id,
            )
            .unwrap();

        if let RespValue::Array(Some(scan_result)) = result {
            // Get next cursor
            if let RespValue::BulkString(Some(cursor_bytes)) = &scan_result[0] {
                let cursor_str = String::from_utf8_lossy(cursor_bytes);
                cursor = cursor_str.parse::<usize>().unwrap();
            }

            // Collect keys
            if let RespValue::Array(Some(keys)) = &scan_result[1] {
                for key in keys {
                    if let RespValue::BulkString(Some(key_bytes)) = key {
                        all_keys.push(String::from_utf8_lossy(key_bytes).to_string());
                    }
                }
            }
        }

        iterations += 1;
        if cursor == 0 || iterations > 100 {
            // Prevent infinite loop in case of bugs
            break;
        }
    }

    // Should have collected all 25 keys
    assert_eq!(all_keys.len(), 25);
    // Cursor should be 0 (iteration complete)
    assert_eq!(cursor, 0);

    // Test SCAN with MATCH
    let result = executor
        .execute(
            "SCAN",
            &[
                Bytes::from("0"),
                Bytes::from("MATCH"),
                Bytes::from("key:1*"),
                Bytes::from("COUNT"),
                Bytes::from("20"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();

    if let RespValue::Array(Some(scan_result)) = result {
        if let RespValue::Array(Some(keys)) = &scan_result[1] {
            // Should return keys matching key:1* (key:1, key:10-19)
            assert!(!keys.is_empty() && keys.len() <= 11);
            // Verify all returned keys match the pattern
            for key in keys {
                if let RespValue::BulkString(Some(key_bytes)) = key {
                    let key_str = String::from_utf8_lossy(key_bytes);
                    assert!(key_str.starts_with("key:1"));
                }
            }
        }
    }
}

#[test]
fn test_set_with_expire_options() {
    let storage = StorageAdapter::new();
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Test SET with EX option
    let result = executor
        .execute(
            "SET",
            &[
                Bytes::from("key1"),
                Bytes::from("value1"),
                Bytes::from("EX"),
                Bytes::from("100"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::ok());

    // Verify TTL was set
    let result = executor
        .execute("TTL", &[Bytes::from("key1")], &mut current_db, client_id)
        .unwrap();
    if let RespValue::Integer(ttl) = result {
        assert!(ttl > 0 && ttl <= 100);
    } else {
        panic!("Expected integer TTL");
    }

    // Test SET with PX option
    let result = executor
        .execute(
            "SET",
            &[
                Bytes::from("key2"),
                Bytes::from("value2"),
                Bytes::from("PX"),
                Bytes::from("50000"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();
    assert_eq!(result, RespValue::ok());

    // Verify PTTL was set
    let result = executor
        .execute("PTTL", &[Bytes::from("key2")], &mut current_db, client_id)
        .unwrap();
    if let RespValue::Integer(pttl) = result {
        assert!(pttl > 0 && pttl <= 50000);
    } else {
        panic!("Expected integer PTTL");
    }
}
