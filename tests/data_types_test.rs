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
