use aikv::command::CommandExecutor;
use aikv::protocol::RespValue;
use aikv::storage::StorageAdapter;
use bytes::Bytes;

#[test]
fn test_list_commands() {
    let storage = StorageAdapter::new();
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
fn test_hash_commands() {
    let storage = StorageAdapter::new();
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
fn test_set_commands() {
    let storage = StorageAdapter::new();
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
    let storage = StorageAdapter::new();
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
    let storage = StorageAdapter::new();
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
