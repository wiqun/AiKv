use aikv::command::CommandExecutor;
use aikv::protocol::RespValue;
use aikv::StorageEngine;
use bytes::Bytes;

#[test]
fn test_script_load_and_exists() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Test SCRIPT LOAD
    let script = "return 'hello world'";
    let result = executor
        .execute(
            "SCRIPT",
            &[Bytes::from("LOAD"), Bytes::from(script)],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // Extract SHA1 from result
    let sha1 = if let RespValue::BulkString(Some(sha)) = result {
        sha
    } else {
        panic!("Expected BulkString with SHA1");
    };

    // Test SCRIPT EXISTS with the returned SHA1
    let result = executor
        .execute(
            "SCRIPT",
            &[Bytes::from("EXISTS"), sha1.clone()],
            &mut current_db,
            client_id,
        )
        .unwrap();

    if let RespValue::Array(Some(arr)) = result {
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0], RespValue::Integer(1));
    } else {
        panic!("Expected Array");
    }

    // Test SCRIPT EXISTS with non-existent SHA1
    let result = executor
        .execute(
            "SCRIPT",
            &[
                Bytes::from("EXISTS"),
                Bytes::from("0000000000000000000000000000000000000000"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();

    if let RespValue::Array(Some(arr)) = result {
        assert_eq!(arr[0], RespValue::Integer(0));
    }
}

#[test]
fn test_eval_simple_script() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Test EVAL with simple return value
    let script = "return 42";
    let result = executor
        .execute(
            "EVAL",
            &[Bytes::from(script), Bytes::from("0")],
            &mut current_db,
            client_id,
        )
        .unwrap();

    assert_eq!(result, RespValue::Integer(42));

    // Test EVAL with string return
    let script = "return 'hello'";
    let result = executor
        .execute(
            "EVAL",
            &[Bytes::from(script), Bytes::from("0")],
            &mut current_db,
            client_id,
        )
        .unwrap();

    if let RespValue::BulkString(Some(value)) = result {
        assert_eq!(String::from_utf8_lossy(&value), "hello");
    } else {
        panic!("Expected BulkString");
    }
}

#[test]
fn test_eval_with_keys_and_argv() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Test EVAL with KEYS
    let script = "return KEYS[1]";
    let result = executor
        .execute(
            "EVAL",
            &[
                Bytes::from(script),
                Bytes::from("1"),
                Bytes::from("testkey"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();

    if let RespValue::BulkString(Some(value)) = result {
        assert_eq!(String::from_utf8_lossy(&value), "testkey");
    } else {
        panic!("Expected BulkString");
    }

    // Test EVAL with ARGV
    let script = "return ARGV[1]";
    let result = executor
        .execute(
            "EVAL",
            &[
                Bytes::from(script),
                Bytes::from("0"),
                Bytes::from("testarg"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();

    if let RespValue::BulkString(Some(value)) = result {
        assert_eq!(String::from_utf8_lossy(&value), "testarg");
    } else {
        panic!("Expected BulkString");
    }
}

#[test]
fn test_eval_redis_call() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Test EVAL with redis.call to SET and GET
    let script = r#"
        redis.call('SET', KEYS[1], ARGV[1])
        return redis.call('GET', KEYS[1])
    "#;

    let result = executor
        .execute(
            "EVAL",
            &[
                Bytes::from(script),
                Bytes::from("1"),
                Bytes::from("mykey"),
                Bytes::from("myvalue"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();

    if let RespValue::BulkString(Some(value)) = result {
        assert_eq!(String::from_utf8_lossy(&value), "myvalue");
    } else {
        panic!("Expected BulkString");
    }

    // Verify the value was actually stored
    let result = executor
        .execute("GET", &[Bytes::from("mykey")], &mut current_db, client_id)
        .unwrap();

    if let RespValue::BulkString(Some(value)) = result {
        assert_eq!(String::from_utf8_lossy(&value), "myvalue");
    }
}

#[test]
fn test_evalsha() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Load a script
    let script = "return 'cached script result'";
    let load_result = executor
        .execute(
            "SCRIPT",
            &[Bytes::from("LOAD"), Bytes::from(script)],
            &mut current_db,
            client_id,
        )
        .unwrap();

    let sha1 = if let RespValue::BulkString(Some(sha)) = load_result {
        sha
    } else {
        panic!("Expected BulkString");
    };

    // Execute the cached script using EVALSHA
    let result = executor
        .execute(
            "EVALSHA",
            &[sha1, Bytes::from("0")],
            &mut current_db,
            client_id,
        )
        .unwrap();

    if let RespValue::BulkString(Some(value)) = result {
        assert_eq!(String::from_utf8_lossy(&value), "cached script result");
    } else {
        panic!("Expected BulkString");
    }
}

#[test]
fn test_evalsha_not_found() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Try to execute a non-existent script
    let result = executor.execute(
        "EVALSHA",
        &[
            Bytes::from("0000000000000000000000000000000000000000"),
            Bytes::from("0"),
        ],
        &mut current_db,
        client_id,
    );

    assert!(result.is_err());
}

#[test]
fn test_script_flush() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Load a script
    let script = "return 1";
    let load_result = executor
        .execute(
            "SCRIPT",
            &[Bytes::from("LOAD"), Bytes::from(script)],
            &mut current_db,
            client_id,
        )
        .unwrap();

    let sha1 = if let RespValue::BulkString(Some(sha)) = load_result {
        sha
    } else {
        panic!("Expected BulkString");
    };

    // Verify script exists
    let result = executor
        .execute(
            "SCRIPT",
            &[Bytes::from("EXISTS"), sha1.clone()],
            &mut current_db,
            client_id,
        )
        .unwrap();

    if let RespValue::Array(Some(arr)) = result {
        assert_eq!(arr[0], RespValue::Integer(1));
    }

    // Flush all scripts
    let result = executor
        .execute(
            "SCRIPT",
            &[Bytes::from("FLUSH")],
            &mut current_db,
            client_id,
        )
        .unwrap();

    assert_eq!(result, RespValue::simple_string("OK"));

    // Verify script no longer exists
    let result = executor
        .execute(
            "SCRIPT",
            &[Bytes::from("EXISTS"), sha1],
            &mut current_db,
            client_id,
        )
        .unwrap();

    if let RespValue::Array(Some(arr)) = result {
        assert_eq!(arr[0], RespValue::Integer(0));
    }
}

#[test]
fn test_script_kill() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Test SCRIPT KILL (should return NOTBUSY since no script is running)
    let result = executor.execute("SCRIPT", &[Bytes::from("KILL")], &mut current_db, client_id);

    assert!(result.is_err());
}
