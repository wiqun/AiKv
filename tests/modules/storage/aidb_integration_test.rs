//! AiDb 统计指标透传与 INFO storage 渲染集成测试.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tempfile::TempDir;

use aikv::server::info::InfoRenderer;
use aikv::server::{ConnectionConfig, ServerSharedState};
use aikv::storage::{
    AiDbEngine, KvStorage, KvStorageAdapter, MemoryEngine, StorageAdapter, StorageEngineKind,
};

#[tokio::test]
async fn test_storage_adapter_and_kv_storage_statistics_flow() {
    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");

    // 1. 验证 StorageAdapter::aidb_statistics 返回 Some(stats)
    let stats = engine
        .aidb_statistics()
        .expect("AiDbEngine must return aidb_statistics");

    // 2. 包装为 KvStorageAdapter，验证透传下传同一个 Arc<Statistics>
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
    let adapter_stats = storage
        .aidb_statistics()
        .expect("KvStorageAdapter must delegate aidb_statistics");

    assert!(
        Arc::ptr_eq(&stats, &adapter_stats),
        "KvStorageAdapter must return the exact same Statistics Arc from underlying engine"
    );

    // 3. 执行写操作，验证底层原子指标直接自增 (AiDbEngine 底层通过 write batch 写入)
    storage.set(0, b"key1", b"val1").await.unwrap();
    assert_eq!(
        stats.operations[aidb::statistics::DbOp::WriteBatch as usize].load(Ordering::Relaxed),
        1,
        "WriteBatch operation count should be 1"
    );
}

#[tokio::test]
async fn test_info_storage_renders_aidb_snapshot() {
    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);

    storage.set(0, b"test_key", b"test_val").await.unwrap();

    let shared = ServerSharedState::new_with_engine(
        ConnectionConfig::default(),
        Arc::clone(&storage),
        6379,
        StorageEngineKind::AiDb,
        Some(dir.path().to_path_buf()),
    );

    let renderer = InfoRenderer::new(&shared, storage.as_ref());

    // 1. 测试单 section "storage" 渲染
    let storage_section = renderer.render(&["storage".to_string()]).await;
    assert!(
        storage_section.contains("# Storage"),
        "must contain # Storage header"
    );
    assert!(
        storage_section.contains("storage_engine:aidb"),
        "must indicate storage_engine:aidb"
    );
    assert!(
        storage_section.contains("aidb_block_cache_hits:"),
        "must contain aidb_block_cache_hits"
    );
    assert!(
        storage_section.contains("aidb_memtable_active_bytes:"),
        "must contain aidb_memtable_active_bytes"
    );
    assert!(
        storage_section.contains("aidb_wal_size_bytes:"),
        "must contain aidb_wal_size_bytes"
    );
    assert!(
        storage_section.contains("aidb_sequence:"),
        "must contain aidb_sequence"
    );

    // 2. 测试默认无参 INFO (render_default) 包含 storage 节
    let default_info = renderer.render(&[]).await;
    assert!(
        default_info.contains("# Storage"),
        "default INFO output must include # Storage section"
    );
}

#[tokio::test]
async fn test_memory_storage_info_renders_clean_memory_engine() {
    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);

    // MemoryEngine 走默认 None
    assert!(
        storage.aidb_statistics().is_none(),
        "MemoryEngine must have None for aidb_statistics"
    );

    let shared = ServerSharedState::new_with_engine(
        ConnectionConfig::default(),
        Arc::clone(&storage),
        6379,
        StorageEngineKind::Memory,
        None,
    );

    let renderer = InfoRenderer::new(&shared, storage.as_ref());
    let storage_section = renderer.render(&["storage".to_string()]).await;

    assert!(storage_section.contains("# Storage"));
    assert!(storage_section.contains("storage_engine:memory"));
    assert!(
        !storage_section.contains("aidb_block_cache_hits:"),
        "MemoryEngine output should not contain aidb specific metrics"
    );
}
