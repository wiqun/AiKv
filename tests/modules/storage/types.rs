//! @component aikv-storage

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aikv::storage::{StoredValue, ValueType};

#[test]
fn test_stored_value_expired() {
    let past = SystemTime::now()
        .checked_sub(Duration::from_secs(10))
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let sv = StoredValue {
        value: ValueType::String(b"x".to_vec()),
        expires_at: Some(past),
    };
    assert!(sv.is_expired());

    let future = SystemTime::now()
        .checked_add(Duration::from_secs(3600))
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let sv2 = StoredValue {
        value: ValueType::String(b"y".to_vec()),
        expires_at: Some(future),
    };
    assert!(!sv2.is_expired());

    let sv3 = StoredValue::string(b"z".to_vec());
    assert!(!sv3.is_expired());
}

#[test]
fn test_stored_value_type_name() {
    use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

    assert_eq!(StoredValue::string(b"".to_vec()).type_name(), "string");
    assert_eq!(
        StoredValue {
            value: ValueType::Hash(HashMap::new()),
            expires_at: None,
        }
        .type_name(),
        "hash"
    );
    assert_eq!(
        StoredValue {
            value: ValueType::List(VecDeque::new()),
            expires_at: None,
        }
        .type_name(),
        "list"
    );
    assert_eq!(
        StoredValue {
            value: ValueType::Set(HashSet::new()),
            expires_at: None,
        }
        .type_name(),
        "set"
    );
    assert_eq!(
        StoredValue {
            value: ValueType::ZSet(BTreeMap::new()),
            expires_at: None,
        }
        .type_name(),
        "zset"
    );
}
