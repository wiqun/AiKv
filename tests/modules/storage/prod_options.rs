//! @component aikv-storage

use aidb::config::Options;
use aikv::storage::{
    server_db_options, testing_db_options, AiDbEngine, KvStorage, KvStorageAdapter,
};
use tempfile::TempDir;

#[test]
fn server_db_options_is_not_for_testing() {
    let prod = server_db_options(false);
    let test = Options::for_testing();

    assert_eq!(prod.memtable_size, 64 * 1024 * 1024);
    assert_eq!(test.memtable_size, 1024 * 1024);
    assert!(prod.background_compaction);
    assert!(!test.background_compaction);
    assert!(!prod.sync_wal);
    assert!(prod.bloom_false_positive_rate > 0.0);
    assert_eq!(test.bloom_false_positive_rate, 0.0);
    assert!(prod.max_wal_size > 0);
    assert_eq!(test.max_wal_size, 0);
}

#[test]
fn server_db_options_validates() {
    server_db_options(false)
        .validate()
        .expect("valid prod options");
    server_db_options(true)
        .validate()
        .expect("valid prod options with sync_wal");
    testing_db_options()
        .validate()
        .expect("valid testing options");
}

#[test]
fn sync_wal_flag_applies_to_server_options() {
    assert!(!server_db_options(false).sync_wal);
    assert!(server_db_options(true).sync_wal);
}

#[tokio::test]
async fn aidb_restart_survives_with_server_options() {
    let dir = TempDir::new().unwrap();
    {
        let engine =
            AiDbEngine::open_with_options(dir.path(), server_db_options(false)).expect("open");
        let storage = KvStorageAdapter::new(engine);
        storage.set(0, b"persist", b"yes").await.unwrap();
        storage.close_engine().await.unwrap();
    }

    let engine =
        AiDbEngine::open_with_options(dir.path(), server_db_options(false)).expect("reopen");
    let storage = KvStorageAdapter::new(engine);
    assert_eq!(
        storage.get(0, b"persist").await.unwrap(),
        Some(b"yes".to_vec())
    );
}

#[tokio::test]
async fn open_and_open_with_server_options_are_equivalent() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    let engine_a = AiDbEngine::open(dir_a.path()).expect("open prod");
    let engine_b = AiDbEngine::open_with_options(dir_b.path(), server_db_options(false))
        .expect("open explicit");
    let storage_a = KvStorageAdapter::new(engine_a);
    let storage_b = KvStorageAdapter::new(engine_b);

    storage_a.set(0, b"k", b"v").await.unwrap();
    storage_b.set(0, b"k", b"v").await.unwrap();
    assert_eq!(
        storage_a.get(0, b"k").await.unwrap(),
        storage_b.get(0, b"k").await.unwrap()
    );
}
