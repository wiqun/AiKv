use std::sync::Arc;
use tempfile::TempDir;

use aikv::storage::{AiDbEngine, KvStorage, KvStorageAdapter};

#[tokio::test]
async fn test_rebuild_counters_duration_recorded() {
    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let adapter = KvStorageAdapter::new(engine);

    // 写入若干数据，确保全库扫描真实发生
    for i in 0..5 {
        let key = format!("rebuild_key_{i}").into_bytes();
        let val = format!("rebuild_val_{i}").into_bytes();
        adapter.set(0, &key, &val).await.unwrap();
    }

    // 显式触发重建计数器 (模拟启动恢复与写批失败重扫)
    adapter.rebuild_counters().await.unwrap();

    let duration_us = adapter.rebuild_counters_duration_us();
    // [ASSUMPTION]: 写入数据后全库 16 个逻辑 DB 扫描完成, 耗时微秒严格大于 0
    assert!(
        duration_us > 0,
        "rebuild_counters_duration_us must be > 0 (got {})",
        duration_us
    );

    // 验证 KvStorage trait 动态分发亦能正确读出该指标
    let storage: Arc<dyn KvStorage> = adapter.clone();
    assert_eq!(storage.rebuild_counters_duration_us(), duration_us);
}
