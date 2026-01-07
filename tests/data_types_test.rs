use aikv::command::CommandExecutor;
use aikv::protocol::RespValue;
use aikv::StorageEngine;
use bytes::Bytes;

#[test]
fn test_list_commands() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // RPUSH
    let args = vec![Bytes::from("mylist"), Bytes::from("world")];
    let result = executor.execute("RPUSH", &args, &mut current_db, client_id);
    assert!(result.is_ok());

    // LPUSH
    let args = vec![Bytes::from("mylist"), Bytes::from("hello")];
    let result = executor.execute("LPUSH", &args, &mut current_db, client_id);
    assert!(result.is_ok());

    // LRANGE
    let args = vec![Bytes::from("mylist"), Bytes::from("0"), Bytes::from("-1")];
    let result = executor.execute("LRANGE", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let Ok(RespValue::Array(Some(items))) = result {
        assert_eq!(items.len(), 2);
    } else {
        panic!("Expected array result");
    }

    // LLEN
    let args = vec![Bytes::from("mylist")];
    let result = executor.execute("LLEN", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(2));
}

#[test]
fn test_linsert_command() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Create a list
    let args = vec![Bytes::from("mylist"), Bytes::from("a"), Bytes::from("c")];
    executor
        .execute("RPUSH", &args, &mut current_db, client_id)
        .unwrap();

    // LINSERT BEFORE
    let args = vec![
        Bytes::from("mylist"),
        Bytes::from("BEFORE"),
        Bytes::from("c"),
        Bytes::from("b"),
    ];
    let result = executor.execute("LINSERT", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(3));

    // Verify list order: a, b, c
    let args = vec![Bytes::from("mylist"), Bytes::from("0"), Bytes::from("-1")];
    let result = executor.execute("LRANGE", &args, &mut current_db, client_id);
    if let Ok(RespValue::Array(Some(items))) = result {
        assert_eq!(items.len(), 3);
    } else {
        panic!("Expected array result");
    }

    // LINSERT AFTER
    let args = vec![
        Bytes::from("mylist"),
        Bytes::from("AFTER"),
        Bytes::from("c"),
        Bytes::from("d"),
    ];
    let result = executor.execute("LINSERT", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(4));

    // LINSERT with non-existent pivot returns -1
    let args = vec![
        Bytes::from("mylist"),
        Bytes::from("BEFORE"),
        Bytes::from("notexist"),
        Bytes::from("x"),
    ];
    let result = executor.execute("LINSERT", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(-1));

    // LINSERT on non-existent key returns 0
    let args = vec![
        Bytes::from("nokey"),
        Bytes::from("BEFORE"),
        Bytes::from("a"),
        Bytes::from("x"),
    ];
    let result = executor.execute("LINSERT", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(0));
}

#[test]
fn test_lmove_command() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Create source list
    let args = vec![
        Bytes::from("src"),
        Bytes::from("a"),
        Bytes::from("b"),
        Bytes::from("c"),
    ];
    executor
        .execute("RPUSH", &args, &mut current_db, client_id)
        .unwrap();

    // LMOVE LEFT RIGHT (pop from left of src, push to right of dst)
    let args = vec![
        Bytes::from("src"),
        Bytes::from("dst"),
        Bytes::from("LEFT"),
        Bytes::from("RIGHT"),
    ];
    let result = executor.execute("LMOVE", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let RespValue::BulkString(Some(value)) = result.unwrap() {
        assert_eq!(value.as_ref(), b"a");
    } else {
        panic!("Expected BulkString result");
    }

    // Verify src has 2 elements
    let args = vec![Bytes::from("src")];
    let result = executor.execute("LLEN", &args, &mut current_db, client_id);
    assert_eq!(result.unwrap(), RespValue::Integer(2));

    // Verify dst has 1 element
    let args = vec![Bytes::from("dst")];
    let result = executor.execute("LLEN", &args, &mut current_db, client_id);
    assert_eq!(result.unwrap(), RespValue::Integer(1));

    // LMOVE RIGHT LEFT (pop from right of src, push to left of dst)
    let args = vec![
        Bytes::from("src"),
        Bytes::from("dst"),
        Bytes::from("RIGHT"),
        Bytes::from("LEFT"),
    ];
    let result = executor.execute("LMOVE", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let RespValue::BulkString(Some(value)) = result.unwrap() {
        assert_eq!(value.as_ref(), b"c");
    } else {
        panic!("Expected BulkString result");
    }

    // LMOVE on non-existent key returns Null
    let args = vec![
        Bytes::from("nokey"),
        Bytes::from("dst"),
        Bytes::from("LEFT"),
        Bytes::from("RIGHT"),
    ];
    let result = executor.execute("LMOVE", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Null);
}

#[test]
fn test_hash_commands() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // HSET
    let args = vec![
        Bytes::from("myhash"),
        Bytes::from("field1"),
        Bytes::from("value1"),
        Bytes::from("field2"),
        Bytes::from("value2"),
    ];
    let result = executor.execute("HSET", &args, &mut current_db, client_id);
    assert!(result.is_ok());

    // HGET
    let args = vec![Bytes::from("myhash"), Bytes::from("field1")];
    let result = executor.execute("HGET", &args, &mut current_db, client_id);
    assert!(result.is_ok());

    // HLEN
    let args = vec![Bytes::from("myhash")];
    let result = executor.execute("HLEN", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(2));

    // HGETALL
    let args = vec![Bytes::from("myhash")];
    let result = executor.execute("HGETALL", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let Ok(RespValue::Array(Some(items))) = result {
        assert_eq!(items.len(), 4); // 2 fields * 2 (field + value)
    } else {
        panic!("Expected array result");
    }
}

#[test]
fn test_hmset_command() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // HMSET - set multiple field-value pairs
    let args = vec![
        Bytes::from("testhash"),
        Bytes::from("field1"),
        Bytes::from("value1"),
        Bytes::from("field2"),
        Bytes::from("value2"),
        Bytes::from("field3"),
        Bytes::from("value3"),
    ];
    let result = executor.execute("HMSET", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    // HMSET should return OK
    if let RespValue::SimpleString(s) = result.unwrap() {
        assert_eq!(s.as_str(), "OK");
    } else {
        panic!("Expected SimpleString OK result");
    }

    // Verify with HLEN
    let args = vec![Bytes::from("testhash")];
    let result = executor.execute("HLEN", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(3));

    // Verify individual fields with HGET
    let args = vec![Bytes::from("testhash"), Bytes::from("field1")];
    let result = executor.execute("HGET", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let RespValue::BulkString(Some(value)) = result.unwrap() {
        assert_eq!(value.as_ref(), b"value1");
    } else {
        panic!("Expected BulkString result");
    }

    // HMSET on existing hash should update/add fields
    let args = vec![
        Bytes::from("testhash"),
        Bytes::from("field1"),
        Bytes::from("newvalue1"),
        Bytes::from("field4"),
        Bytes::from("value4"),
    ];
    let result = executor.execute("HMSET", &args, &mut current_db, client_id);
    assert!(result.is_ok());

    // Verify updated field
    let args = vec![Bytes::from("testhash"), Bytes::from("field1")];
    let result = executor.execute("HGET", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let RespValue::BulkString(Some(value)) = result.unwrap() {
        assert_eq!(value.as_ref(), b"newvalue1");
    } else {
        panic!("Expected BulkString result");
    }

    // Verify total fields count
    let args = vec![Bytes::from("testhash")];
    let result = executor.execute("HLEN", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(4));
}

#[test]
fn test_hscan_command() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Create a hash with multiple fields
    let args = vec![
        Bytes::from("scanhash"),
        Bytes::from("field1"),
        Bytes::from("value1"),
        Bytes::from("field2"),
        Bytes::from("value2"),
        Bytes::from("field3"),
        Bytes::from("value3"),
        Bytes::from("anotherfield"),
        Bytes::from("anothervalue"),
    ];
    executor
        .execute("HSET", &args, &mut current_db, client_id)
        .unwrap();

    // HSCAN with cursor 0 (start of iteration)
    let args = vec![Bytes::from("scanhash"), Bytes::from("0")];
    let result = executor.execute("HSCAN", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let RespValue::Array(Some(items)) = result.unwrap() {
        assert_eq!(items.len(), 2); // [cursor, [fields]]
                                    // Check fields array
        if let RespValue::Array(Some(fields)) = &items[1] {
            assert_eq!(fields.len(), 8); // 4 fields * 2 (field + value)
        } else {
            panic!("Expected array of fields");
        }
    } else {
        panic!("Expected array result");
    }

    // HSCAN with COUNT option
    let args = vec![
        Bytes::from("scanhash"),
        Bytes::from("0"),
        Bytes::from("COUNT"),
        Bytes::from("2"),
    ];
    let result = executor.execute("HSCAN", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let RespValue::Array(Some(items)) = result.unwrap() {
        assert_eq!(items.len(), 2);
        // Check next cursor is not 0 (more fields to iterate)
        if let RespValue::BulkString(Some(cursor)) = &items[0] {
            let cursor_val: usize = String::from_utf8_lossy(cursor).parse().unwrap();
            assert_eq!(cursor_val, 2); // Next cursor should be 2
        }
        // Check fields array has 4 items (2 fields * 2)
        if let RespValue::Array(Some(fields)) = &items[1] {
            assert_eq!(fields.len(), 4);
        } else {
            panic!("Expected array of fields");
        }
    } else {
        panic!("Expected array result");
    }

    // HSCAN with MATCH option
    let args = vec![
        Bytes::from("scanhash"),
        Bytes::from("0"),
        Bytes::from("MATCH"),
        Bytes::from("field*"),
    ];
    let result = executor.execute("HSCAN", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let RespValue::Array(Some(items)) = result.unwrap() {
        // Check fields array - should only have field1, field2, field3
        if let RespValue::Array(Some(fields)) = &items[1] {
            assert_eq!(fields.len(), 6); // 3 fields * 2 (field + value)
        } else {
            panic!("Expected array of fields");
        }
    } else {
        panic!("Expected array result");
    }

    // HSCAN on non-existent key should return empty result
    let args = vec![Bytes::from("nonexistent"), Bytes::from("0")];
    let result = executor.execute("HSCAN", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let RespValue::Array(Some(items)) = result.unwrap() {
        assert_eq!(items.len(), 2);
        if let RespValue::BulkString(Some(cursor)) = &items[0] {
            assert_eq!(cursor.as_ref(), b"0");
        }
        if let RespValue::Array(Some(fields)) = &items[1] {
            assert!(fields.is_empty());
        }
    } else {
        panic!("Expected array result");
    }
}

#[test]
fn test_set_commands() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // SADD
    let args = vec![
        Bytes::from("myset"),
        Bytes::from("member1"),
        Bytes::from("member2"),
        Bytes::from("member3"),
    ];
    let result = executor.execute("SADD", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(3));

    // SCARD
    let args = vec![Bytes::from("myset")];
    let result = executor.execute("SCARD", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(3));

    // SISMEMBER
    let args = vec![Bytes::from("myset"), Bytes::from("member1")];
    let result = executor.execute("SISMEMBER", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(1));

    // SREM
    let args = vec![Bytes::from("myset"), Bytes::from("member2")];
    let result = executor.execute("SREM", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(1));

    // SCARD after removal
    let args = vec![Bytes::from("myset")];
    let result = executor.execute("SCARD", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(2));
}

#[test]
fn test_zset_commands() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // ZADD
    let args = vec![
        Bytes::from("myzset"),
        Bytes::from("1"),
        Bytes::from("one"),
        Bytes::from("2"),
        Bytes::from("two"),
        Bytes::from("3"),
        Bytes::from("three"),
    ];
    let result = executor.execute("ZADD", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(3));

    // ZCARD
    let args = vec![Bytes::from("myzset")];
    let result = executor.execute("ZCARD", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(3));

    // ZSCORE
    let args = vec![Bytes::from("myzset"), Bytes::from("two")];
    let result = executor.execute("ZSCORE", &args, &mut current_db, client_id);
    assert!(result.is_ok());

    // ZRANK
    let args = vec![Bytes::from("myzset"), Bytes::from("two")];
    let result = executor.execute("ZRANK", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(1)); // 0-indexed

    // ZRANGE
    let args = vec![Bytes::from("myzset"), Bytes::from("0"), Bytes::from("-1")];
    let result = executor.execute("ZRANGE", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let Ok(RespValue::Array(Some(items))) = result {
        assert_eq!(items.len(), 3);
    } else {
        panic!("Expected array result");
    }
}

#[test]
fn test_set_operations() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Create set1
    let args = vec![
        Bytes::from("set1"),
        Bytes::from("a"),
        Bytes::from("b"),
        Bytes::from("c"),
    ];
    executor
        .execute("SADD", &args, &mut current_db, client_id)
        .unwrap();

    // Create set2
    let args = vec![
        Bytes::from("set2"),
        Bytes::from("b"),
        Bytes::from("c"),
        Bytes::from("d"),
    ];
    executor
        .execute("SADD", &args, &mut current_db, client_id)
        .unwrap();

    // SUNION
    let args = vec![Bytes::from("set1"), Bytes::from("set2")];
    let result = executor.execute("SUNION", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let Ok(RespValue::Array(Some(items))) = result {
        assert_eq!(items.len(), 4); // a, b, c, d
    } else {
        panic!("Expected array result");
    }

    // SINTER
    let args = vec![Bytes::from("set1"), Bytes::from("set2")];
    let result = executor.execute("SINTER", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let Ok(RespValue::Array(Some(items))) = result {
        assert_eq!(items.len(), 2); // b, c
    } else {
        panic!("Expected array result");
    }

    // SDIFF
    let args = vec![Bytes::from("set1"), Bytes::from("set2")];
    let result = executor.execute("SDIFF", &args, &mut current_db, client_id);
    assert!(result.is_ok());
    if let Ok(RespValue::Array(Some(items))) = result {
        assert_eq!(items.len(), 1); // a
    } else {
        panic!("Expected array result");
    }
}

// ================= NEW STRING COMMANDS TESTS =================

#[test]
fn test_incr_decr_commands() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // INCR on non-existent key (starts from 0)
    let result = executor.execute(
        "INCR",
        &[Bytes::from("counter")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(1));

    // INCR again
    let result = executor.execute(
        "INCR",
        &[Bytes::from("counter")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(2));

    // DECR
    let result = executor.execute(
        "DECR",
        &[Bytes::from("counter")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(1));

    // DECR below 0
    let result = executor.execute(
        "DECR",
        &[Bytes::from("counter")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(0));

    let result = executor.execute(
        "DECR",
        &[Bytes::from("counter")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(-1));
}

#[test]
fn test_incrby_decrby_commands() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // INCRBY
    let result = executor.execute(
        "INCRBY",
        &[Bytes::from("counter"), Bytes::from("10")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(10));

    // INCRBY again
    let result = executor.execute(
        "INCRBY",
        &[Bytes::from("counter"), Bytes::from("5")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(15));

    // DECRBY
    let result = executor.execute(
        "DECRBY",
        &[Bytes::from("counter"), Bytes::from("3")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(12));
}

#[test]
fn test_incrbyfloat_command() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // INCRBYFLOAT on non-existent key
    let result = executor.execute(
        "INCRBYFLOAT",
        &[Bytes::from("floatkey"), Bytes::from("10.5")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());

    // INCRBYFLOAT again
    let result = executor.execute(
        "INCRBYFLOAT",
        &[Bytes::from("floatkey"), Bytes::from("0.1")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());

    // INCRBYFLOAT with negative
    let result = executor.execute(
        "INCRBYFLOAT",
        &[Bytes::from("floatkey"), Bytes::from("-5.2")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
}

#[test]
fn test_getrange_setrange_commands() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // SET a string
    executor
        .execute(
            "SET",
            &[Bytes::from("mykey"), Bytes::from("Hello World")],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // GETRANGE
    let result = executor.execute(
        "GETRANGE",
        &[Bytes::from("mykey"), Bytes::from("0"), Bytes::from("4")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::BulkString(Some(value)) = result.unwrap() {
        assert_eq!(value.as_ref(), b"Hello");
    } else {
        panic!("Expected BulkString result");
    }

    // GETRANGE with negative indices
    let result = executor.execute(
        "GETRANGE",
        &[Bytes::from("mykey"), Bytes::from("-5"), Bytes::from("-1")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::BulkString(Some(value)) = result.unwrap() {
        assert_eq!(value.as_ref(), b"World");
    } else {
        panic!("Expected BulkString result");
    }

    // SETRANGE
    let result = executor.execute(
        "SETRANGE",
        &[Bytes::from("mykey"), Bytes::from("6"), Bytes::from("Redis")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(11));

    // Verify
    let result = executor.execute("GET", &[Bytes::from("mykey")], &mut current_db, client_id);
    if let Ok(RespValue::BulkString(Some(value))) = result {
        assert_eq!(value.as_ref(), b"Hello Redis");
    } else {
        panic!("Expected BulkString result");
    }
}

#[test]
fn test_getex_getdel_commands() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // SET a value
    executor
        .execute(
            "SET",
            &[Bytes::from("mykey"), Bytes::from("Hello")],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // GETEX with EX option
    let result = executor.execute(
        "GETEX",
        &[Bytes::from("mykey"), Bytes::from("EX"), Bytes::from("100")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::BulkString(Some(value)) = result.unwrap() {
        assert_eq!(value.as_ref(), b"Hello");
    } else {
        panic!("Expected BulkString result");
    }

    // GETDEL
    let result = executor.execute(
        "GETDEL",
        &[Bytes::from("mykey")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::BulkString(Some(value)) = result.unwrap() {
        assert_eq!(value.as_ref(), b"Hello");
    } else {
        panic!("Expected BulkString result");
    }

    // Key should be deleted
    let result = executor.execute("GET", &[Bytes::from("mykey")], &mut current_db, client_id);
    assert!(result.is_ok());
    match result.unwrap() {
        RespValue::BulkString(None) | RespValue::Null => {}
        _ => panic!("Expected null result"),
    }
}

#[test]
fn test_setnx_setex_psetex_commands() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // SETNX on non-existent key
    let result = executor.execute(
        "SETNX",
        &[Bytes::from("mykey"), Bytes::from("Hello")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(1));

    // SETNX on existing key
    let result = executor.execute(
        "SETNX",
        &[Bytes::from("mykey"), Bytes::from("World")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(0));

    // Verify value unchanged
    let result = executor.execute("GET", &[Bytes::from("mykey")], &mut current_db, client_id);
    if let Ok(RespValue::BulkString(Some(value))) = result {
        assert_eq!(value.as_ref(), b"Hello");
    } else {
        panic!("Expected BulkString result");
    }

    // SETEX
    let result = executor.execute(
        "SETEX",
        &[
            Bytes::from("exkey"),
            Bytes::from("10"),
            Bytes::from("value"),
        ],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::SimpleString(s) = result.unwrap() {
        assert_eq!(s.as_str(), "OK");
    } else {
        panic!("Expected OK result");
    }

    // PSETEX
    let result = executor.execute(
        "PSETEX",
        &[
            Bytes::from("pexkey"),
            Bytes::from("10000"),
            Bytes::from("value"),
        ],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::SimpleString(s) = result.unwrap() {
        assert_eq!(s.as_str(), "OK");
    } else {
        panic!("Expected OK result");
    }
}

// ================= NEW LIST COMMANDS TESTS =================

#[test]
fn test_lpos_command() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Create a list
    executor
        .execute(
            "RPUSH",
            &[
                Bytes::from("mylist"),
                Bytes::from("a"),
                Bytes::from("b"),
                Bytes::from("c"),
                Bytes::from("b"),
                Bytes::from("d"),
                Bytes::from("b"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // LPOS - find first occurrence
    let result = executor.execute(
        "LPOS",
        &[Bytes::from("mylist"), Bytes::from("b")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(1));

    // LPOS with RANK 2 - find second occurrence
    let result = executor.execute(
        "LPOS",
        &[
            Bytes::from("mylist"),
            Bytes::from("b"),
            Bytes::from("RANK"),
            Bytes::from("2"),
        ],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(3));

    // LPOS with COUNT - find all occurrences
    let result = executor.execute(
        "LPOS",
        &[
            Bytes::from("mylist"),
            Bytes::from("b"),
            Bytes::from("COUNT"),
            Bytes::from("0"),
        ],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::Array(Some(arr)) = result.unwrap() {
        assert_eq!(arr.len(), 3);
    } else {
        panic!("Expected array result");
    }

    // LPOS - element not found
    let result = executor.execute(
        "LPOS",
        &[Bytes::from("mylist"), Bytes::from("x")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Null);
}

// ================= NEW SET COMMANDS TESTS =================

#[test]
fn test_sscan_command() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Create a set
    executor
        .execute(
            "SADD",
            &[
                Bytes::from("myset"),
                Bytes::from("member1"),
                Bytes::from("member2"),
                Bytes::from("member3"),
                Bytes::from("other"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // SSCAN
    let result = executor.execute(
        "SSCAN",
        &[Bytes::from("myset"), Bytes::from("0")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::Array(Some(items)) = result.unwrap() {
        assert_eq!(items.len(), 2);
        if let RespValue::Array(Some(members)) = &items[1] {
            assert_eq!(members.len(), 4);
        }
    } else {
        panic!("Expected array result");
    }

    // SSCAN with MATCH
    let result = executor.execute(
        "SSCAN",
        &[
            Bytes::from("myset"),
            Bytes::from("0"),
            Bytes::from("MATCH"),
            Bytes::from("member*"),
        ],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::Array(Some(items)) = result.unwrap() {
        if let RespValue::Array(Some(members)) = &items[1] {
            assert_eq!(members.len(), 3);
        }
    } else {
        panic!("Expected array result");
    }
}

#[test]
fn test_smove_command() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Create source set
    executor
        .execute(
            "SADD",
            &[
                Bytes::from("src"),
                Bytes::from("a"),
                Bytes::from("b"),
                Bytes::from("c"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // Create destination set
    executor
        .execute(
            "SADD",
            &[Bytes::from("dst"), Bytes::from("x"), Bytes::from("y")],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // SMOVE
    let result = executor.execute(
        "SMOVE",
        &[Bytes::from("src"), Bytes::from("dst"), Bytes::from("b")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(1));

    // Verify source
    let result = executor.execute("SCARD", &[Bytes::from("src")], &mut current_db, client_id);
    assert_eq!(result.unwrap(), RespValue::Integer(2));

    // Verify destination
    let result = executor.execute("SCARD", &[Bytes::from("dst")], &mut current_db, client_id);
    assert_eq!(result.unwrap(), RespValue::Integer(3));

    // SMOVE non-existent member
    let result = executor.execute(
        "SMOVE",
        &[Bytes::from("src"), Bytes::from("dst"), Bytes::from("z")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(0));
}

// ================= NEW SORTED SET COMMANDS TESTS =================

#[test]
fn test_zscan_command() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Create a sorted set
    executor
        .execute(
            "ZADD",
            &[
                Bytes::from("myzset"),
                Bytes::from("1"),
                Bytes::from("one"),
                Bytes::from("2"),
                Bytes::from("two"),
                Bytes::from("3"),
                Bytes::from("three"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // ZSCAN
    let result = executor.execute(
        "ZSCAN",
        &[Bytes::from("myzset"), Bytes::from("0")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::Array(Some(items)) = result.unwrap() {
        assert_eq!(items.len(), 2);
        if let RespValue::Array(Some(members)) = &items[1] {
            assert_eq!(members.len(), 6); // 3 members * 2 (member + score)
        }
    } else {
        panic!("Expected array result");
    }
}

#[test]
fn test_zpopmin_zpopmax_commands() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Create a sorted set
    executor
        .execute(
            "ZADD",
            &[
                Bytes::from("myzset"),
                Bytes::from("1"),
                Bytes::from("one"),
                Bytes::from("2"),
                Bytes::from("two"),
                Bytes::from("3"),
                Bytes::from("three"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // ZPOPMIN
    let result = executor.execute(
        "ZPOPMIN",
        &[Bytes::from("myzset")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::Array(Some(items)) = result.unwrap() {
        assert_eq!(items.len(), 2);
        if let RespValue::BulkString(Some(member)) = &items[0] {
            assert_eq!(member.as_ref(), b"one");
        }
    } else {
        panic!("Expected array result");
    }

    // ZPOPMAX
    let result = executor.execute(
        "ZPOPMAX",
        &[Bytes::from("myzset")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::Array(Some(items)) = result.unwrap() {
        assert_eq!(items.len(), 2);
        if let RespValue::BulkString(Some(member)) = &items[0] {
            assert_eq!(member.as_ref(), b"three");
        }
    } else {
        panic!("Expected array result");
    }

    // Verify only "two" remains
    let result = executor.execute(
        "ZCARD",
        &[Bytes::from("myzset")],
        &mut current_db,
        client_id,
    );
    assert_eq!(result.unwrap(), RespValue::Integer(1));
}

#[test]
fn test_zrangebylex_zrevrangebylex_zlexcount_commands() {
    let storage = StorageEngine::new_memory(16);
    let executor = CommandExecutor::new(storage);
    let mut current_db = 0;
    let client_id = 1;

    // Create a sorted set with same score for lex ordering
    executor
        .execute(
            "ZADD",
            &[
                Bytes::from("myzset"),
                Bytes::from("0"),
                Bytes::from("a"),
                Bytes::from("0"),
                Bytes::from("b"),
                Bytes::from("0"),
                Bytes::from("c"),
                Bytes::from("0"),
                Bytes::from("d"),
                Bytes::from("0"),
                Bytes::from("e"),
            ],
            &mut current_db,
            client_id,
        )
        .unwrap();

    // ZRANGEBYLEX
    let result = executor.execute(
        "ZRANGEBYLEX",
        &[Bytes::from("myzset"), Bytes::from("[b"), Bytes::from("[d")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::Array(Some(items)) = result.unwrap() {
        assert_eq!(items.len(), 3); // b, c, d
    } else {
        panic!("Expected array result");
    }

    // ZRANGEBYLEX with exclusive range
    let result = executor.execute(
        "ZRANGEBYLEX",
        &[Bytes::from("myzset"), Bytes::from("(a"), Bytes::from("(e")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::Array(Some(items)) = result.unwrap() {
        assert_eq!(items.len(), 3); // b, c, d
    } else {
        panic!("Expected array result");
    }

    // ZREVRANGEBYLEX
    let result = executor.execute(
        "ZREVRANGEBYLEX",
        &[Bytes::from("myzset"), Bytes::from("[d"), Bytes::from("[b")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    if let RespValue::Array(Some(items)) = result.unwrap() {
        assert_eq!(items.len(), 3); // d, c, b (reversed)
    } else {
        panic!("Expected array result");
    }

    // ZLEXCOUNT
    let result = executor.execute(
        "ZLEXCOUNT",
        &[Bytes::from("myzset"), Bytes::from("-"), Bytes::from("+")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(5));

    // ZLEXCOUNT with range
    let result = executor.execute(
        "ZLEXCOUNT",
        &[Bytes::from("myzset"), Bytes::from("[b"), Bytes::from("[d")],
        &mut current_db,
        client_id,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), RespValue::Integer(3));
}
