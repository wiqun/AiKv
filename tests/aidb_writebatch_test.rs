/// Test to demonstrate AiDb WriteBatch atomic behavior
///
/// This test shows how AiDb's WriteBatch provides true atomic writes
/// with WAL durability guarantees.
#[cfg(test)]
mod aidb_writebatch_tests {
    use aikv::storage::{AiDbStorageAdapter, BatchOp};
    use bytes::Bytes;
    use tempfile::TempDir;

    #[test]
    fn test_aidb_write_batch_atomicity() {
        // Create a temporary directory for the test
        let temp_dir = TempDir::new().unwrap();
        let storage = AiDbStorageAdapter::new(temp_dir.path(), 1).unwrap();

        // Prepare batch operations
        let operations = vec![
            ("key1".to_string(), BatchOp::Set(Bytes::from("value1"))),
            ("key2".to_string(), BatchOp::Set(Bytes::from("value2"))),
            ("key3".to_string(), BatchOp::Set(Bytes::from("value3"))),
        ];

        // Write batch atomically
        storage.write_batch(0, operations).unwrap();

        // Verify all keys are present
        let val1 = storage.get_from_db(0, "key1").unwrap();
        assert_eq!(val1, Some(Bytes::from("value1")));

        let val2 = storage.get_from_db(0, "key2").unwrap();
        assert_eq!(val2, Some(Bytes::from("value2")));

        let val3 = storage.get_from_db(0, "key3").unwrap();
        assert_eq!(val3, Some(Bytes::from("value3")));
    }

    #[test]
    fn test_aidb_write_batch_with_deletes() {
        let temp_dir = TempDir::new().unwrap();
        let storage = AiDbStorageAdapter::new(temp_dir.path(), 1).unwrap();

        // Set initial values
        storage
            .set_in_db(0, "key1".to_string(), Bytes::from("initial1"))
            .unwrap();
        storage
            .set_in_db(0, "key2".to_string(), Bytes::from("initial2"))
            .unwrap();
        storage
            .set_in_db(0, "key3".to_string(), Bytes::from("initial3"))
            .unwrap();

        // Batch operation: update key1, delete key2, keep key3
        let operations = vec![
            ("key1".to_string(), BatchOp::Set(Bytes::from("updated1"))),
            ("key2".to_string(), BatchOp::Delete),
        ];

        storage.write_batch(0, operations).unwrap();

        // Verify results
        assert_eq!(
            storage.get_from_db(0, "key1").unwrap(),
            Some(Bytes::from("updated1"))
        );
        assert_eq!(storage.get_from_db(0, "key2").unwrap(), None);
        assert_eq!(
            storage.get_from_db(0, "key3").unwrap(),
            Some(Bytes::from("initial3"))
        );
    }

    #[test]
    fn test_aidb_write_batch_empty() {
        let temp_dir = TempDir::new().unwrap();
        let storage = AiDbStorageAdapter::new(temp_dir.path(), 1).unwrap();

        // Empty batch should succeed
        let operations = vec![];
        let result = storage.write_batch(0, operations);
        assert!(result.is_ok());
    }

    #[test]
    fn test_aidb_write_batch_large() {
        let temp_dir = TempDir::new().unwrap();
        let storage = AiDbStorageAdapter::new(temp_dir.path(), 1).unwrap();

        // Create a large batch (100 operations)
        let mut operations = Vec::new();
        for i in 0..100 {
            let key = format!("batch_key_{}", i);
            let value = format!("batch_value_{}", i);
            operations.push((key, BatchOp::Set(Bytes::from(value))));
        }

        // Write batch atomically
        storage.write_batch(0, operations).unwrap();

        // Verify all keys are present
        for i in 0..100 {
            let key = format!("batch_key_{}", i);
            let expected_value = format!("batch_value_{}", i);
            let actual_value = storage.get_from_db(0, &key).unwrap();
            assert_eq!(actual_value, Some(Bytes::from(expected_value)));
        }
    }

    #[test]
    fn test_aidb_write_batch_overwrite() {
        let temp_dir = TempDir::new().unwrap();
        let storage = AiDbStorageAdapter::new(temp_dir.path(), 1).unwrap();

        // Set initial value
        storage
            .set_in_db(0, "key".to_string(), Bytes::from("old_value"))
            .unwrap();

        // Batch overwrites with new value
        let operations = vec![("key".to_string(), BatchOp::Set(Bytes::from("new_value")))];

        storage.write_batch(0, operations).unwrap();

        // Verify overwrite succeeded
        assert_eq!(
            storage.get_from_db(0, "key").unwrap(),
            Some(Bytes::from("new_value"))
        );
    }
}
