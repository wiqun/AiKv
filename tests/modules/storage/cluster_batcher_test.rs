//! 集群自适应写凑批微窗口测试
//! @component aikv-storage

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};
use aikv::storage::cluster_batcher::{collect_batch_items, WriteBatchItem};

#[tokio::test]
async fn test_adaptive_batch_aggregation_under_concurrent_writes() {
    let (tx, mut rx) = mpsc::channel(100);

    let (ack_tx, _ack_rx) = oneshot::channel();
    let first = WriteBatchItem::Put {
        key: b"k_first".to_vec(),
        value: b"v_first".to_vec(),
        ack: ack_tx,
    };

    // 模拟并发任务在当前协程之后极短时间内投递 20 条写请求
    let tx_clone = tx.clone();
    tokio::spawn(async move {
        for i in 0..20 {
            let (ack_tx, _ack_rx) = oneshot::channel();
            let _ = tx_clone
                .send(WriteBatchItem::Put {
                    key: format!("k_{i}").into_bytes(),
                    value: format!("v_{i}").into_bytes(),
                    ack: ack_tx,
                })
                .await;
        }
    });

    let items = collect_batch_items(&mut rx, first).await;

    // 自适应微窗口应让步等待并发客户端完成入队, 聚合出大批次 (>= 20) 而非孤立的单项
    assert!(
        items.len() >= 20,
        "自适应微窗口必须聚合并发协程投递的写入项 (目标 >= 20), 实际仅收集: {}",
        items.len()
    );
}

#[tokio::test]
async fn test_adaptive_batch_aggregation_multiple_producers() {
    let (tx, mut rx) = mpsc::channel(200);

    let (ack_tx, _ack_rx) = oneshot::channel();
    let first = WriteBatchItem::Put {
        key: b"head".to_vec(),
        value: b"head_val".to_vec(),
        ack: ack_tx,
    };

    // 启动 4 个并发生产者协程, 每个协程投递 8 条写入
    for p in 0..4 {
        let producer_tx = tx.clone();
        tokio::spawn(async move {
            for i in 0..8 {
                let (ack_tx, _ack_rx) = oneshot::channel();
                let _ = producer_tx
                    .send(WriteBatchItem::Put {
                        key: format!("k_{p}_{i}").into_bytes(),
                        value: format!("v_{p}_{i}").into_bytes(),
                        ack: ack_tx,
                    })
                    .await;
            }
        });
    }

    let items = collect_batch_items(&mut rx, first).await;

    // 1 (first) + 32 (来自 4 个生产者的写入) = 33 项
    // 自适应微窗口让步后应聚合出大部分或全部已就绪生产者项
    assert!(
        items.len() >= 20,
        "多并发生产者下自适应微窗口必须聚合大批量写入项, 实际收集: {}",
        items.len()
    );
}

/// 未接入集群 (单机) 时写落在本地引擎. `enqueue_chain` 以 `enqueue_batch` 返回为同 key 脚本写的顺序点,
/// 所以后入队的写必须覆盖先入队的写, 与两个 handle 谁先 `wait` 无关.
#[tokio::test]
async fn local_fallback_writes_are_ordered_by_enqueue_not_by_wait() {
    use aikv::storage::cluster_adapter::ClusterDataAdapter;
    use aikv::storage::{AdapterWriteOp, AiDbEngine, StorageAdapter};

    let dir = tempfile::TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
    let adapter = ClusterDataAdapter::new(engine, ClusterDataAdapter::DEFAULT_EAGER_FLUSH);
    let put = |v: &[u8]| {
        vec![AdapterWriteOp::Put {
            key: b"k".to_vec(),
            value: v.to_vec(),
        }]
    };

    let first = adapter.enqueue_batch(put(b"old")).await.unwrap();
    let second = adapter.enqueue_batch(put(b"new")).await.unwrap();
    second.wait().await.unwrap();
    first.wait().await.unwrap();

    assert_eq!(
        adapter.get(b"k".to_vec()).await.unwrap().as_deref(),
        Some(b"new".as_ref()),
        "先入队的旧值在 wait 时才落盘, 覆盖了后入队的新值"
    );
}

/// 配对的正向用例: 同样的入队, 但按入队顺序 wait, 最终也是后入队的值 (证明上面的断言能区分结果).
#[tokio::test]
async fn local_fallback_writes_wait_in_order_end_with_latest() {
    use aikv::storage::cluster_adapter::ClusterDataAdapter;
    use aikv::storage::{AdapterWriteOp, AiDbEngine, StorageAdapter};

    let dir = tempfile::TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
    let adapter = ClusterDataAdapter::new(engine, ClusterDataAdapter::DEFAULT_EAGER_FLUSH);
    let put = |v: &[u8]| {
        vec![AdapterWriteOp::Put {
            key: b"k".to_vec(),
            value: v.to_vec(),
        }]
    };
    let first = adapter.enqueue_batch(put(b"old")).await.unwrap();
    let second = adapter.enqueue_batch(put(b"new")).await.unwrap();
    first.wait().await.unwrap();
    second.wait().await.unwrap();
    assert_eq!(
        adapter.get(b"k".to_vec()).await.unwrap().as_deref(),
        Some(b"new".as_ref())
    );
}

/// 与 `main.rs` 单机相同的存储栈 (`KvStorageAdapter` -> `ClusterDataAdapter` -> `AiDbEngine`) 上, 写脚本必须成功且可读到.
#[tokio::test]
async fn standalone_script_writes_work_through_the_cluster_adapter_stack() {
    use aikv::command::CommandRouter;
    use aikv::protocol::RespValue;
    use aikv::storage::cluster_adapter::ClusterDataAdapter;
    use aikv::storage::{AiDbEngine, KvStorageAdapter, StorageAdapter};

    let dir = tempfile::TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
    let local: Arc<dyn StorageAdapter> = engine;
    let adapter: Arc<dyn StorageAdapter> =
        ClusterDataAdapter::new(local, ClusterDataAdapter::DEFAULT_EAGER_FLUSH);
    let storage = KvStorageAdapter::open(adapter, None).await.unwrap();
    let router = CommandRouter::new(storage);

    let mut db = 0;
    let script = Bytes::from_static(b"local n=tonumber(redis.call('get',KEYS[1])) or 0; redis.call('set',KEYS[1],n+1); return n");
    for expect in 0..3 {
        let args = [script.clone(), Bytes::from_static(b"1"), Bytes::from_static(b"k")];
        let got = router.execute("EVAL", &args, &mut db).await.unwrap();
        assert_eq!(got, RespValue::Integer(expect));
    }
    let got = router
        .execute("GET", &[Bytes::from_static(b"k")], &mut db)
        .await
        .unwrap();
    assert_eq!(got, RespValue::BulkString(Some(Bytes::from_static(b"3"))));
}
